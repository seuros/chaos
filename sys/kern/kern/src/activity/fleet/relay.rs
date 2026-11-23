//! Private question mailbox shared by opted-in owners and the resident.
//! The opaque row id is a per-waiter generation, not a turn id. Database writes
//! request delivery; only the process holding the receiver can record success.
use chaos_ipc::request_user_input::{RequestUserInputEvent, RequestUserInputResponse};
use serde_json::Value;
use sqlx::{PgPool, types::Json};
use std::time::Duration;
use uuid::Uuid;
pub(crate) mod history;

pub async fn initialize(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chaos_mobile_questions (
        id uuid PRIMARY KEY, host text NOT NULL, session_id text NOT NULL,
        prompt jsonb NOT NULL, proposed jsonb, accepted jsonb, source text,
        seen timestamptz NOT NULL DEFAULT now(), created timestamptz NOT NULL DEFAULT now())",
    )
    .execute(pool)
    .await?;
    sqlx::query("ALTER TABLE chaos_mobile_questions ADD COLUMN IF NOT EXISTS history jsonb NOT NULL DEFAULT '[]'").execute(pool).await?;
    sqlx::query("DELETE FROM chaos_mobile_questions WHERE seen < now()-interval '1 day'")
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn history(pool: &PgPool, id: Uuid) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, Json<Value>>(
        "SELECT history FROM chaos_mobile_questions WHERE id=$1 AND seen >= now()-interval '1 day'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map(|v| v.map(|j| j.0))
}

pub async fn inbox(pool: &PgPool) -> Result<Vec<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, Json<Value>>(
        "SELECT jsonb_build_object('id',id,'host',host,'session',session_id,'prompt',prompt,
         'answer',accepted,'source',source,'status', CASE WHEN accepted IS NOT NULL THEN 'answered'
         WHEN seen < now()-interval '12 seconds' THEN 'unavailable'
         WHEN proposed IS NOT NULL THEN 'confirming' ELSE 'pending' END)
         FROM chaos_mobile_questions WHERE created > now()-interval '1 day' OR seen > now()-interval '12 seconds'
         ORDER BY (accepted IS NULL AND seen >= now()-interval '12 seconds') DESC, created DESC LIMIT 256")
        .fetch_all(pool).await.map(|rows| rows.into_iter().map(|j| j.0).collect())
}

pub async fn propose(
    pool: &PgPool,
    id: Uuid,
    answer: RequestUserInputResponse,
) -> Result<bool, sqlx::Error> {
    let row: Option<Json<RequestUserInputEvent>> =
        sqlx::query_scalar("SELECT prompt FROM chaos_mobile_questions WHERE id=$1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some(Json(prompt)) = row else {
        return Ok(false);
    };
    if !valid(&prompt, &answer) {
        return Ok(false);
    }
    // A retry (even a different answer) cannot overwrite the first proposal.
    sqlx::query(
        "UPDATE chaos_mobile_questions SET proposed=$2 WHERE id=$1 AND proposed IS NULL
        AND accepted IS NULL AND seen >= now()-interval '12 seconds'",
    )
    .bind(id)
    .bind(Json(answer))
    .execute(pool)
    .await?;
    Ok(true)
}

fn valid(prompt: &RequestUserInputEvent, answer: &RequestUserInputResponse) -> bool {
    !prompt.questions.is_empty()
        && prompt.questions.len() == answer.answers.len()
        && prompt.questions.iter().all(|q| {
            !q.is_secret
                && answer.answers.get(&q.id).is_some_and(|a| {
                    a.answers.len() == 1
                        && a.answers[0].len() <= 2048
                        && !a.answers[0].trim().is_empty()
                        && (q.is_other
                            || q.options
                                .as_ref()
                                .is_some_and(|opts| opts.iter().any(|o| o.label == a.answers[0])))
                })
        })
}

/// Best effort publication. Failure cannot prevent a desktop answer or
/// overwrite it. Never publish a secret question to the shared database.
pub(crate) struct Mailbox {
    pool: PgPool,
    id: Uuid,
    history: std::collections::VecDeque<history::Entry>,
}

impl Mailbox {
    pub(crate) fn for_prompt(
        session: chaos_ipc::ProcessId,
        prompt: &RequestUserInputEvent,
    ) -> Option<Self> {
        if std::env::var("CHAOS_MOBILE_OWNER").as_deref() != Ok("1")
            || prompt.questions.iter().any(|q| q.is_secret)
        {
            return None;
        }
        Some(Self {
            pool: chaos_vfs::root().ok()?.postgres_pool()?,
            id: Uuid::new_v4(),
            history: history::snapshot(session),
        })
    }

    pub(crate) async fn proposal(
        &self,
        session: &str,
        prompt: &RequestUserInputEvent,
    ) -> RequestUserInputResponse {
        loop {
            // Also re-register after a resident starts late. The server, never an
            // owner, provisions this table. Disconnected waits simply lose their lease.
            let result = sqlx::query_scalar::<_, Option<Json<RequestUserInputResponse>>>(
                "INSERT INTO chaos_mobile_questions(id,host,session_id,prompt,history) VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT(id) DO UPDATE SET seen=now() RETURNING proposed",
            )
            .bind(self.id)
            .bind(super::local_host())
            .bind(session)
            .bind(Json(prompt))
            .bind(Json(&self.history))
            .fetch_one(&self.pool)
            .await;
            if let Ok(Some(Json(response))) = result
                && valid(prompt, &response)
            {
                return response;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub(crate) async fn confirm(
        &self,
        session: &str,
        prompt: &RequestUserInputEvent,
        answer: &RequestUserInputResponse,
        mobile: bool,
    ) {
        let write = sqlx::query("INSERT INTO chaos_mobile_questions(id,host,session_id,prompt,accepted,source,history)
            VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(id) DO UPDATE SET accepted=excluded.accepted, source=excluded.source,
            proposed=NULL, seen=now() WHERE chaos_mobile_questions.accepted IS NULL")
            .bind(self.id).bind(super::local_host()).bind(session).bind(Json(prompt)).bind(Json(answer))
            .bind(if mobile {"phone"} else {"desktop"}).bind(Json(&self.history)).execute(&self.pool);
        if !matches!(
            tokio::time::timeout(Duration::from_secs(2), write).await,
            Ok(Ok(_))
        ) {
            tracing::warn!("could not confirm question delivery to mobile resident");
        }
    }
}
