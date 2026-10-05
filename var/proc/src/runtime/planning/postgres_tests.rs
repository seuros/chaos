//! Run with TEST_DATABASE_URL against a disposable PostgreSQL database.
//! Each test uses its own schema and independent pool connections.
use super::tests::{add, create, request};
use super::*;

async fn fixture() -> anyhow::Result<(
    RuntimeDbHandle,
    PgPool,
    PgPool,
    String,
    PlanningActor,
    String,
)> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?;
    let schema = format!("planning_test_{}", Uuid::new_v4().simple());
    // schema is a fixed prefix plus a locally generated UUID, never user input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await?;
    let options = PgConnectOptions::from_str(&url)?.options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../../db/migrate/postgres/0020_planning.sql"
    ))
    .execute(&pool)
    .await?;
    sqlx::raw_sql(include_str!(
        "../../../db/migrate/postgres/0021_planning_content.sql"
    ))
    .execute(&pool)
    .await?;
    sqlx::raw_sql(include_str!(
        "../../../db/migrate/postgres/0022_planning_consolidations.sql"
    ))
    .execute(&pool)
    .await?;
    let db =
        RuntimeDbHandle::from_postgres_pool(PathBuf::from("/unused"), "test".into(), pool.clone());
    let actor = PlanningActor {
        session: "one".into(),
        installation: "one".into(),
    };
    let workspace = db.planning_create_workspace("ecosystem").await?.id;
    Ok((db, pool, admin, schema, actor, workspace))
}

async fn cleanup(pool: PgPool, admin: PgPool, schema: &str) -> anyhow::Result<()> {
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn postgres_bodies_and_clarifications() -> anyhow::Result<()> {
    let (db, pool, admin, schema, actor, workspace) = fixture().await?;
    super::tests::content_roundtrip(&db, &actor, &workspace).await?;
    super::consolidation::tests::roundtrip(&db, &actor, &workspace).await?;
    cleanup(pool, admin, &schema).await
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn postgres_consolidation_races_publish_once() -> anyhow::Result<()> {
    let (db, pool, admin, schema, actor, workspace) = fixture().await?;
    super::consolidation::tests::publication_races(&db, &actor, &workspace).await?;
    cleanup(pool, admin, &schema).await
}

async fn edge(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    plan: &str,
    parent: &str,
    child: &str,
    insert: bool,
) -> anyhow::Result<()> {
    let revision = db.planning_read(plan, 0).await?.plan.revision;
    let change = if insert {
        PlanChange::Link {
            parent: parent.into(),
            child: child.into(),
        }
    } else {
        PlanChange::Unlink {
            parent: parent.into(),
            child: child.into(),
        }
    };
    db.planning_mutate(actor, &request(Some(plan), Some(revision), change))
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; creates and removes an isolated schema"]
async fn postgres_diamonds_depth_repair_and_raw_sql_guards() -> anyhow::Result<()> {
    let (db, pool, admin, schema, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace).await?.id;
    for revision in 1..=4 {
        add(&db, &actor, &plan, revision).await?;
    }
    let ids: Vec<_> = db
        .planning_read(&plan, 0)
        .await?
        .tasks
        .into_iter()
        .map(|node| node.task.id)
        .collect();
    for (from, to) in [
        ("T1", "T2"),
        ("T1", "T3"),
        ("T2", "T4"),
        ("T3", "T4"),
        ("T1", "T4"),
    ] {
        edge(&db, &actor, &plan, from, to, true).await?;
    }
    assert_eq!(db.planning_graph(&plan, None, 0).await?.items.len(), 5);
    assert_eq!(
        db.planning_graph(&plan, Some("T2"), 0).await?.items.len(),
        1
    );
    assert!(
        db.planning_graph(&plan, Some("T4"), 0)
            .await?
            .items
            .is_empty()
    );
    let paths = || {
        sqlx::query("SELECT min_depth,path_count::text AS count FROM planning_paths WHERE ancestor_id=$1 AND descendant_id=$2")
        .bind(&ids[0]).bind(&ids[3])
    };
    let row = paths().fetch_one(&pool).await?;
    assert_eq!(row.try_get::<i64, _>("min_depth")?, 1);
    assert_eq!(row.try_get::<String, _>("count")?, "3");
    edge(&db, &actor, &plan, "T1", "T4", false).await?;
    let row = paths().fetch_one(&pool).await?;
    assert_eq!(row.try_get::<i64, _>("min_depth")?, 2);
    assert_eq!(row.try_get::<String, _>("count")?, "2");
    edge(&db, &actor, &plan, "T2", "T4", false).await?;
    assert_eq!(
        paths()
            .fetch_one(&pool)
            .await?
            .try_get::<String, _>("count")?,
        "1"
    );
    assert!(
        sqlx::query("INSERT INTO planning_edges VALUES($1,$2,$3)")
            .bind(&plan)
            .bind(&ids[3])
            .bind(&ids[0])
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE planning_tasks SET parent_id=id WHERE id=$1")
            .bind(&ids[0])
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE planning_tasks SET parent_id=$1 WHERE id=$2")
        .bind(&ids[0])
        .bind(&ids[1])
        .execute(&pool)
        .await?;
    assert!(
        sqlx::query("UPDATE planning_tasks SET parent_id=$1 WHERE id=$2")
            .bind(&ids[1])
            .bind(&ids[0])
            .execute(&pool)
            .await
            .is_err()
    );
    let other = create(&db, &actor, &workspace).await?.id;
    add(&db, &actor, &other, 1).await?;
    let foreign = db.planning_read(&other, 0).await?.tasks.remove(0).task.id;
    assert!(
        sqlx::query("INSERT INTO planning_edges VALUES($1,$2,$3)")
            .bind(&plan)
            .bind(&ids[0])
            .bind(&foreign)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("SELECT * FROM planning_validate_paths($1)")
            .bind(&plan)
            .fetch_all(&pool)
            .await?
            .is_empty()
    );
    sqlx::query("DELETE FROM planning_paths WHERE ancestor_id=$1 AND descendant_id=$2")
        .bind(&ids[0])
        .bind(&ids[3])
        .execute(&pool)
        .await?;
    assert_eq!(
        sqlx::query("SELECT * FROM planning_validate_paths($1)")
            .bind(&plan)
            .fetch_all(&pool)
            .await?
            .len(),
        1
    );
    assert_eq!(db.planning_validate_graph(&plan, 0).await?.items.len(), 1);
    let revision = db.planning_read(&plan, 0).await?.plan.revision;
    assert!(
        db.planning_rebuild_graph(&plan, revision - 1)
            .await
            .is_err()
    );
    db.planning_rebuild_graph(&plan, revision).await?;
    assert!(db.planning_validate_graph(&plan, 0).await?.items.is_empty());
    assert!(
        sqlx::query("SELECT * FROM planning_validate_paths($1)")
            .bind(&plan)
            .fetch_all(&pool)
            .await?
            .is_empty()
    );
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    assert!(
        sqlx::query("SELECT planning_rebuild_paths($1)")
            .bind(&plan)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await?;
    drop(db);
    cleanup(pool, admin, &schema).await
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; creates and removes an isolated schema"]
async fn postgres_concurrent_edges_revisions_and_cross_machine_resume() -> anyhow::Result<()> {
    let (db, pool, admin, schema, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace).await?.id;
    add(&db, &actor, &plan, 1).await?;
    add(&db, &actor, &plan, 2).await?;
    let tasks = db.planning_read(&plan, 0).await?.tasks;
    let one = tasks[0].task.id.clone();
    let two = tasks[1].task.id.clone();
    let insert = |parent: String, child: String| {
        let pool = pool.clone();
        let plan = plan.clone();
        async move {
            sqlx::query("INSERT INTO planning_edges VALUES($1,$2,$3)")
                .bind(plan)
                .bind(parent)
                .bind(child)
                .execute(&pool)
                .await
        }
    };
    let (a, b) = tokio::join!(insert(one.clone(), two.clone()), insert(two, one));
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(
        sqlx::query("SELECT * FROM planning_validate_paths($1)")
            .bind(&plan)
            .fetch_all(&pool)
            .await?
            .is_empty()
    );
    let second = RuntimeDbHandle::from_postgres_pool(
        PathBuf::from("/another-machine"),
        "test".into(),
        pool.clone(),
    );
    let actor2 = PlanningActor {
        session: "two".into(),
        installation: "two".into(),
    };
    let first = request(
        Some(&plan),
        None,
        PlanChange::Transition {
            task: "T1".into(),
            task_revision: 1,
            event: TaskEvent::Start,
            reason: "machine one".into(),
        },
    );
    let next = request(
        Some(&plan),
        None,
        PlanChange::Transition {
            task: "T1".into(),
            task_revision: 1,
            event: TaskEvent::Cancel,
            reason: "machine two".into(),
        },
    );
    let (a, b) = tokio::join!(
        db.planning_mutate(&actor, &first),
        second.planning_mutate(&actor2, &next)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        db.planning_read(&plan, 0).await?,
        second.planning_read(&plan, 0).await?
    );
    db.planning_attach(&actor.session, Some(&plan)).await?;
    assert_eq!(
        second.planning_attachment(&actor.session).await?,
        Some(plan.clone())
    );
    let revision = db.planning_read(&plan, 0).await?.plan.revision;
    let mutation = request(
        Some(&plan),
        Some(revision),
        PlanChange::AddTask {
            title: "retry".into(),
            body: String::new(),
            parent: None,
            position: 3,
        },
    );
    let (a, b) = tokio::join!(
        db.planning_mutate(&actor, &mutation),
        second.planning_mutate(&actor, &mutation)
    );
    assert_eq!(a?, b?);
    drop(second);
    drop(db);
    cleanup(pool, admin, &schema).await
}
