//! A self-hosted token relay: `ZERON_EDGE_TOKEN` is a shared secret and
//! `ZERON_USER_ID` names the user. Its own test binary because it sets
//! process environment.

use zeron_engine::{AuthState, Engine, EngineConfig, HarnessId, WorkspaceScope};

const SECRET: &str = "4f1c0d2a9b7e6f5a3c8d1e0f2a4b6c8d";

fn config(data_dir: &std::path::Path) -> EngineConfig {
    EngineConfig {
        data_dir: data_dir.to_path_buf(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: Some(SECRET.into()),
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        org_id: Some("personal".into()),
        workos_client_id: None,
    }
}

#[tokio::test]
async fn zeron_user_id_keeps_a_shared_token_out_of_the_identity() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());

    // Without ZERON_USER_ID the bearer is the identity (dev relays).
    unsafe { std::env::remove_var("ZERON_USER_ID") };
    let auth = Engine::build_auth(&config).await;
    assert_eq!(auth.user_id().as_deref(), Some(SECRET));

    unsafe { std::env::set_var("ZERON_USER_ID", "owner") };
    let auth = Engine::build_auth(&config).await;
    assert_eq!(
        Engine::initial_workspace_scope(&auth),
        WorkspaceScope::Development
    );
    assert_eq!(auth.access_token().await.as_deref(), Ok(SECRET));
    assert_eq!(auth.user_id().as_deref(), Some("owner"));
    let AuthState::SignedIn { user, .. } = auth.state() else {
        panic!("dev auth is signed in");
    };
    assert_eq!((user.id.as_str(), user.email.as_str()), ("owner", "owner"));

    let profile = Engine::resolve_profile(&config, &auth, WorkspaceScope::Development)
        .unwrap()
        .expect("development profile");
    assert_eq!(profile.store_root(), dir.path().join("orgs/personal/owner"));
    assert_eq!((profile.org_id(), profile.user_id()), ("personal", "owner"));
}
