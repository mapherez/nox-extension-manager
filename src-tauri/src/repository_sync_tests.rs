use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    Arc,
};
use std::thread;
use std::time::Duration;

const ID: &str = "NoxBookmarks";
const MANIFEST: &[u8] = br#"{"manifest_version":3,"name":"Hero Bookmarks","version":"1.1.1"}"#;
const SCRIPT: &[u8] = b"// trusted repository content";

struct RepositoryServer {
    url: String,
    stop: Arc<AtomicBool>,
    fail_download: Arc<AtomicBool>,
    directory_requests: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RepositoryServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let fail_download = Arc::new(AtomicBool::new(false));
        let directory_requests = Arc::new(AtomicUsize::new(0));
        let (done, fail, listing_requests) = (
            stop.clone(),
            fail_download.clone(),
            directory_requests.clone(),
        );
        let worker = thread::spawn(move || {
            while !done.load(AtomicOrdering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("Test server failed: {error}"),
                };
                // Accepted sockets inherit non-blocking mode on Windows.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                while !request.windows(4).any(|value| value == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .split('?')
                    .next()
                    .unwrap_or_default();
                let (status, body) = if path == "/repos/owner/repo/contents/extensions" {
                    listing_requests.fetch_add(1, AtomicOrdering::SeqCst);
                    (
                        200,
                        serde_json::json!([{
                            "name": ID, "path": format!("extensions/{ID}"),
                            "type": "dir", "sha": "directory", "size": 0,
                        }])
                        .to_string()
                        .into_bytes(),
                    )
                } else if path == format!("/repos/owner/repo/contents/extensions/{ID}") {
                    let files: Vec<_> = [("manifest.json", MANIFEST), ("content.js", SCRIPT)]
                        .iter()
                        .map(|(name, bytes)| {
                            serde_json::json!({
                                "name": name, "path": format!("extensions/{ID}/{name}"),
                                "type": "file", "sha": blob_oid(bytes), "size": bytes.len(),
                            })
                        })
                        .collect();
                    (200, serde_json::to_vec(&files).unwrap())
                } else if path.ends_with("/manifest.json") {
                    (200, MANIFEST.to_vec())
                } else if path.ends_with("/content.js") && fail.swap(false, AtomicOrdering::SeqCst)
                {
                    (503, b"temporary download failure".to_vec())
                } else if path.ends_with("/content.js") {
                    (200, SCRIPT.to_vec())
                } else {
                    (404, b"unknown route".to_vec())
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            url,
            stop,
            fail_download,
            directory_requests,
            worker: Some(worker),
        }
    }

    fn client(&self) -> RepositoryClient {
        RepositoryClient {
            config: RepositoryConfig {
                repository: "owner/repo".into(),
                branch: "master".into(),
                extensions_path: "extensions".into(),
                repository_url: "https://github.com/owner/repo".into(),
            },
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            api_base: self.url.clone(),
            raw_base: self.url.clone(),
        }
    }
}

impl Drop for RepositoryServer {
    fn drop(&mut self) {
        self.stop.store(true, AtomicOrdering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn blob_oid(bytes: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", bytes.len()).as_bytes());
    digest.update(bytes);
    hex::encode(digest.finalize())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn sync_rediscovers_missing_entries_and_adopts_identical_downloads() {
    let server = RepositoryServer::start();
    let temp = tempfile::tempdir().unwrap();
    let paths = StoragePaths::from_root(temp.path().join("extensions"));
    paths.ensure().unwrap();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        state.extensions.remove(ID);
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
    });
    assert_eq!(server.directory_requests.load(AtomicOrdering::SeqCst), 2);
    assert!(state.extensions[ID].problem.is_none());
    assert_eq!(
        state.extensions[ID].active_version.as_deref(),
        Some("1.1.1")
    );
    assert_eq!(
        fs::read(paths.extension_dir(ID).join("content.js")).unwrap(),
        SCRIPT
    );
}

#[test]
fn sync_restores_a_deleted_installed_directory_and_prepares_reload() {
    let server = RepositoryServer::start();
    let temp = tempfile::tempdir().unwrap();
    let paths = StoragePaths::from_root(temp.path().join("extensions"));
    paths.ensure().unwrap();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        fs::remove_dir_all(paths.extension_dir(ID)).unwrap();
        let chrome = ChromeState {
            extensions: BTreeMap::from([(
                ID.into(),
                crate::model::ChromeInstallRecord {
                    chrome_extension_id: "abcdefghijklmnopabcdefghijklmnop".into(),
                    version: "1.0.0".into(),
                    installed_at: String::new(),
                },
            )]),
        };
        client.sync(&paths, &mut state, &chrome).await.unwrap();
    });
    assert!(state.extensions[ID].problem.is_none());
    assert_eq!(
        state.extensions[ID].pending_version.as_deref(),
        Some("1.1.1")
    );
    assert_eq!(
        hash_directory(&paths.extension_dir(ID)).unwrap(),
        state.extensions[ID].active_hashes
    );
    assert_eq!(
        hash_directory(&paths.pending_dir(ID, "1.1.1")).unwrap(),
        state.extensions[ID].pending_hashes
    );
}

#[test]
fn sync_retries_failed_downloads() {
    let server = RepositoryServer::start();
    server.fail_download.store(true, AtomicOrdering::SeqCst);
    let temp = tempfile::tempdir().unwrap();
    let paths = StoragePaths::from_root(temp.path().join("extensions"));
    paths.ensure().unwrap();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        assert!(state.extensions[ID].problem.is_some());
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
    });
    assert_eq!(server.directory_requests.load(AtomicOrdering::SeqCst), 2);
    assert!(state.extensions[ID].problem.is_none());
    assert!(paths.extension_dir(ID).join("manifest.json").is_file());
}

#[test]
fn sync_preserves_manual_edits_and_explicit_repair_backs_them_up() {
    let server = RepositoryServer::start();
    let temp = tempfile::tempdir().unwrap();
    let paths = StoragePaths::from_root(temp.path().join("extensions"));
    paths.ensure().unwrap();
    let mut state = RepositoryState::default();
    let client = server.client();
    let unrelated = paths.root.join("OtherExtension");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("keep.txt"), "leave this alone").unwrap();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        fs::write(paths.extension_dir(ID).join("content.js"), "manual edits").unwrap();
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        assert!(state.extensions[ID]
            .problem
            .as_deref()
            .unwrap()
            .starts_with("Local files have changed"));
        assert_eq!(
            fs::read_to_string(paths.extension_dir(ID).join("content.js")).unwrap(),
            "manual edits"
        );
        client
            .restore_files(&paths, &mut state, &ChromeState::default(), ID)
            .await
            .unwrap();
    });
    assert!(state.extensions[ID].problem.is_none());
    assert_eq!(
        fs::read(paths.extension_dir(ID).join("content.js")).unwrap(),
        SCRIPT
    );
    let backups: Vec<_> = fs::read_dir(paths.backup_dir(ID))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        fs::read_to_string(backups[0].path().join("content.js")).unwrap(),
        "manual edits"
    );
    assert_eq!(
        fs::read_to_string(unrelated.join("keep.txt")).unwrap(),
        "leave this alone"
    );
}
