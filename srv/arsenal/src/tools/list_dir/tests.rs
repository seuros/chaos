use super::*;
use pretty_assertions::assert_eq;

fn entry_values(entries: &[DirEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry.name,
                "name": entry.display_name,
                "kind": entry.kind.as_str(),
                "depth": entry.depth,
            })
        })
        .collect()
}

#[tokio::test]
async fn lists_directory_entries() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let dir_path = temp.path();

    let sub_dir = dir_path.join("nested");
    tokio::fs::create_dir(&sub_dir)
        .await
        .expect("create sub dir");

    let deeper_dir = sub_dir.join("deeper");
    tokio::fs::create_dir(&deeper_dir)
        .await
        .expect("create deeper dir");

    tokio::fs::write(dir_path.join("entry.txt"), b"content")
        .await
        .expect("write file");
    tokio::fs::write(sub_dir.join("child.txt"), b"child")
        .await
        .expect("write child");
    tokio::fs::write(deeper_dir.join("grandchild.txt"), b"grandchild")
        .await
        .expect("write grandchild");

    use std::os::unix::fs::symlink;
    let link_path = dir_path.join("link");
    symlink(dir_path.join("entry.txt"), &link_path).expect("create symlink");

    let (entries, truncated) = list_dir_slice(dir_path, 1, 20, 3)
        .await
        .expect("list directory");

    let expected = vec![
        serde_json::json!({"path":"entry.txt","name":"entry.txt","kind":"file","depth":0}),
        serde_json::json!({"path":"link","name":"link","kind":"symlink","depth":0}),
        serde_json::json!({"path":"nested","name":"nested","kind":"directory","depth":0}),
        serde_json::json!({"path":"nested/child.txt","name":"child.txt","kind":"file","depth":1}),
        serde_json::json!({"path":"nested/deeper","name":"deeper","kind":"directory","depth":1}),
        serde_json::json!({
            "path": "nested/deeper/grandchild.txt",
            "name": "grandchild.txt",
            "kind": "file",
            "depth": 2,
        }),
    ];

    assert_eq!(entry_values(&entries), expected);
    assert!(!truncated);

    let output = execute_structured(&serde_json::json!({
        "dir_path": dir_path,
        "limit": 20,
        "depth": 3
    }))
    .await
    .expect("structured directory listing");
    assert_eq!(output["entries"], serde_json::json!(expected));
    assert_eq!(output["truncated"], false);
}

#[tokio::test]
async fn errors_when_offset_exceeds_entries() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let dir_path = temp.path();
    tokio::fs::create_dir(dir_path.join("nested"))
        .await
        .expect("create sub dir");

    let Err(err) = list_dir_slice(dir_path, 10, 1, 2).await else {
        panic!("offset exceeds entries");
    };
    assert_eq!(err, "offset exceeds directory entry count");
}

#[tokio::test]
async fn respects_depth_parameter() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let dir_path = temp.path();
    let nested = dir_path.join("nested");
    let deeper = nested.join("deeper");
    tokio::fs::create_dir(&nested).await.expect("create nested");
    tokio::fs::create_dir(&deeper).await.expect("create deeper");
    tokio::fs::write(dir_path.join("root.txt"), b"root")
        .await
        .expect("write root");
    tokio::fs::write(nested.join("child.txt"), b"child")
        .await
        .expect("write nested");
    tokio::fs::write(deeper.join("grandchild.txt"), b"deep")
        .await
        .expect("write deeper");

    let (entries_depth_one, _) = list_dir_slice(dir_path, 1, 10, 1)
        .await
        .expect("list depth 1");
    assert_eq!(
        entry_values(&entries_depth_one),
        vec![
            serde_json::json!({"path":"nested","name":"nested","kind":"directory","depth":0}),
            serde_json::json!({"path":"root.txt","name":"root.txt","kind":"file","depth":0})
        ]
    );

    let (entries_depth_two, _) = list_dir_slice(dir_path, 1, 20, 2)
        .await
        .expect("list depth 2");
    assert_eq!(
        entry_values(&entries_depth_two),
        vec![
            serde_json::json!({"path":"nested","name":"nested","kind":"directory","depth":0}),
            serde_json::json!({
                "path": "nested/child.txt",
                "name": "child.txt",
                "kind": "file",
                "depth": 1,
            }),
            serde_json::json!({
                "path": "nested/deeper",
                "name": "deeper",
                "kind": "directory",
                "depth": 1,
            }),
            serde_json::json!({"path":"root.txt","name":"root.txt","kind":"file","depth":0})
        ]
    );
}

#[tokio::test]
async fn paginates_in_sorted_order() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let dir_path = temp.path();

    let dir_a = dir_path.join("a");
    let dir_b = dir_path.join("b");
    tokio::fs::create_dir(&dir_a).await.expect("create a");
    tokio::fs::create_dir(&dir_b).await.expect("create b");

    tokio::fs::write(dir_a.join("a_child.txt"), b"a")
        .await
        .expect("write a child");
    tokio::fs::write(dir_b.join("b_child.txt"), b"b")
        .await
        .expect("write b child");

    let (first_page, first_truncated) = list_dir_slice(dir_path, 1, 2, 2)
        .await
        .expect("list page one");
    assert_eq!(
        entry_values(&first_page),
        vec![
            serde_json::json!({"path":"a","name":"a","kind":"directory","depth":0}),
            serde_json::json!({
                "path": "a/a_child.txt",
                "name": "a_child.txt",
                "kind": "file",
                "depth": 1,
            }),
        ]
    );
    assert!(first_truncated);

    let (second_page, second_truncated) = list_dir_slice(dir_path, 3, 2, 2)
        .await
        .expect("list page two");
    assert_eq!(
        entry_values(&second_page),
        vec![
            serde_json::json!({"path":"b","name":"b","kind":"directory","depth":0}),
            serde_json::json!({"path":"b/b_child.txt","name":"b_child.txt","kind":"file","depth":1})
        ]
    );
    assert!(!second_truncated);
}

#[tokio::test]
async fn rejects_unknown_arguments() {
    let result = execute_structured(&serde_json::json!({
        "dir_path": "/tmp",
        "limitt": 10
    }))
    .await;

    let err = result.expect_err("unknown field should fail");
    assert!(err.contains("unknown field `limitt`"));
}
