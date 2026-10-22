//! TASK-29 / TASK-7: untrusted projects run on global config only, and the
//! trust store is a protected path.

use harness_config::ini;
use harness_config::paths::DataDir;
use harness_config::trust::{TrustStatus, TrustStore};

#[test]
fn untrusted_project_still_works_with_global_config() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("repo");
    std::fs::create_dir_all(project.join(".harness")).unwrap();
    std::fs::write(project.join(".harness/agents.yaml"), "malicious: true\n").unwrap();

    let global = ini::load_str("[general]\ndefault_model = opus\n", None).unwrap();
    let mut global = global;
    global.general.data_dir = tmp.path().join("data");
    let data = DataDir::from_config(&global).open().unwrap();
    let store = TrustStore::from_data_dir(&data);

    assert!(matches!(
        store.status(&project).unwrap(),
        TrustStatus::Untrusted { .. }
    ));
    assert_eq!(store.trusted_config_dir(&project).unwrap(), None);
    assert_eq!(
        global.general.default_model, "opus",
        "global config is unaffected"
    );
}

#[test]
fn trust_store_is_protected() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path()).open().unwrap();
    let store = TrustStore::from_data_dir(&data);
    let protected = data.protected_paths(None);
    assert!(protected.iter().any(|p| store.path().starts_with(p)));
    assert!(protected.contains(&store.path().to_path_buf()));
}
