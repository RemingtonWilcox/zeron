//! Agent artifacts: a chat's agent asks to show a workspace file or a
//! loopback page (`ShowArtifact`), and every window watching that chat
//! (`WatchArtifacts`, on this device or through another one) opens it beside
//! the chat.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest::Url;
use tokio::sync::broadcast;
use zeron_proto::Artifact;

/// Per-chat fan-out to the windows showing that chat. Nothing is retained:
/// a window that opens the chat later only sees later artifacts.
#[derive(Clone, Default)]
pub struct Artifacts(Arc<Mutex<HashMap<String, broadcast::Sender<Artifact>>>>);

impl Artifacts {
    pub fn watch(&self, chat_id: &str) -> BoxStream<'static, serde_json::Value> {
        let mut chats = lock(&self.0);
        chats.retain(|_, sender| sender.receiver_count() > 0);
        let receiver = chats
            .entry(chat_id.to_owned())
            .or_insert_with(|| broadcast::channel(8).0)
            .subscribe();
        futures::stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(artifact) => return Some((serde_json::to_value(artifact).ok()?, receiver)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed()
    }

    /// Hands `artifact` to the windows watching `chat_id`; how many there were.
    pub fn show(&self, chat_id: &str, artifact: Artifact) -> usize {
        lock(&self.0)
            .get(chat_id)
            .and_then(|sender| sender.send(artifact).ok())
            .unwrap_or(0)
    }
}

/// An http(s) page on this device's loopback, plus its preview-proxy address
/// when discovery already serves that port to other devices.
pub fn page(
    target: &str,
    previews: Option<&zeron_preview::PreviewService>,
) -> Result<Artifact, String> {
    let url = Url::parse(target)
        .ok()
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && loopback(url)
        })
        .ok_or_else(|| {
            format!(
                "{target} is not a local page: show opens files in this chat's workspace and \
                 http(s) addresses on localhost, *.localhost or a loopback IP"
            )
        })?;
    let preview = previews.and_then(|previews| {
        let catalog = previews.catalog();
        let port = url.port_or_known_default()?;
        let service = catalog
            .local_services()
            .into_iter()
            .find(|service| service.port == port)?;
        let mut preview = Url::parse(&service.url(catalog.snapshot().proxy_port)).ok()?;
        preview.set_path(url.path());
        preview.set_query(url.query());
        preview.set_fragment(url.fragment());
        Some(preview.into())
    });
    Ok(Artifact::Url {
        url: url.into(),
        preview,
    })
}

fn loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host.ends_with(".localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_must_be_loopback_http() {
        for target in [
            "http://localhost:5173/",
            "https://localhost/a?b=1#c",
            "http://app.localhost:3000/",
            "http://127.0.0.1:8000/index.html",
            "http://[::1]:8080/",
        ] {
            assert!(
                matches!(page(target, None), Ok(Artifact::Url { preview: None, .. })),
                "{target}"
            );
        }
        for target in [
            "https://example.com/",
            "http://localhost.example.com/",
            "http://10.0.0.2:3000/",
            "http://user:pass@localhost:3000/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://localhost/",
        ] {
            let error = page(target, None).unwrap_err();
            assert!(error.contains("not a local page"), "{target}: {error}");
        }
    }

    #[tokio::test]
    async fn only_windows_watching_the_chat_receive_its_artifacts() {
        let artifacts = Artifacts::default();
        let file = Artifact::File {
            path: "out/chart.svg".into(),
        };
        assert_eq!(artifacts.show("chat", file.clone()), 0);
        let mut first = artifacts.watch("chat");
        let mut second = artifacts.watch("chat");
        let mut other = artifacts.watch("other");
        assert_eq!(artifacts.show("chat", file.clone()), 2);
        let expected = serde_json::to_value(&file).unwrap();
        assert_eq!(first.next().await.unwrap(), expected);
        assert_eq!(second.next().await.unwrap(), expected);
        drop(second);
        assert_eq!(artifacts.show("chat", file), 1);
        assert!(
            futures::FutureExt::now_or_never(other.next()).is_none(),
            "another chat's window must not see it"
        );
    }
}
