use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    Arc,
};
use std::thread;
use std::time::Duration;

const ID: &str = "SampleExtension";
const MANIFEST: &[u8] = br#"{"manifest_version":3,"name":"Sample Extension","version":"1.1.1"}"#;
const SCRIPT: &[u8] = b"// trusted repository content";

struct RepositoryServer {
    url: String,
    stop: Arc<AtomicBool>,
    fail_download: Arc<AtomicBool>,
    directory_requests: Arc<AtomicUsize>,
    payload_requests: Arc<AtomicUsize>,
    file_listing_requests: Arc<AtomicUsize>,
    unexpected_requests: Arc<AtomicUsize>,
    manifest: Arc<std::sync::Mutex<Vec<u8>>>,
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
        let payload_requests = Arc::new(AtomicUsize::new(0));
        let file_listing_requests = Arc::new(AtomicUsize::new(0));
        let unexpected_requests = Arc::new(AtomicUsize::new(0));
        let manifest = Arc::new(std::sync::Mutex::new(MANIFEST.to_vec()));
        let (payloads, file_listings, unexpected, remote_manifest) = (
            payload_requests.clone(),
            file_listing_requests.clone(),
            unexpected_requests.clone(),
            manifest.clone(),
        );
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
                let manifest = remote_manifest.lock().unwrap().clone();
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
                    file_listings.fetch_add(1, AtomicOrdering::SeqCst);
                    let files: Vec<_> = [
                        ("manifest.json", manifest.as_slice()),
                        ("content.js", SCRIPT),
                    ]
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
                    (200, manifest)
                } else if path.ends_with("/content.js") {
                    payloads.fetch_add(1, AtomicOrdering::SeqCst);
                    if fail.swap(false, AtomicOrdering::SeqCst) {
                        (503, b"temporary download failure".to_vec())
                    } else {
                        (200, SCRIPT.to_vec())
                    }
                } else {
                    unexpected.fetch_add(1, AtomicOrdering::SeqCst);
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
            payload_requests,
            file_listing_requests,
            unexpected_requests,
            manifest,
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

fn storage() -> (tempfile::TempDir, StoragePaths) {
    let temp = tempfile::tempdir().unwrap();
    let paths = StoragePaths::from_root(temp.path().join("extensions"));
    paths.ensure().unwrap();
    (temp, paths)
}

#[test]
fn sync_rediscovers_catalog_entries_without_downloading_or_adopting_files() {
    let server = RepositoryServer::start();
    let (_temp, paths) = storage();
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
    let entry = &state.extensions[ID];
    assert_eq!(entry.name, "Sample Extension");
    assert_eq!(entry.remote_version, "1.1.1");
    assert!(entry.available);
    assert!(entry.problem.is_none());
    assert!(entry.active_version.is_none());
    assert!(entry.pending_version.is_none());
    assert!(!paths.extension_dir(ID).exists());
    assert_eq!(server.file_listing_requests.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(server.payload_requests.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(server.unexpected_requests.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(fs::read_dir(&paths.staging).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&paths.updates).unwrap().count(), 0);
}

#[test]
fn sync_never_restores_a_missing_directory_based_on_old_manager_install_records() {
    let server = RepositoryServer::start();
    let (_temp, paths) = storage();
    let mut state = RepositoryState::default();
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
    runtime()
        .block_on(server.client().sync(&paths, &mut state, &chrome))
        .unwrap();
    assert!(state.extensions[ID].pending_version.is_none());
    assert!(state.extensions[ID].active_version.is_none());
    assert!(!paths.extension_dir(ID).exists());
    assert_eq!(server.payload_requests.load(AtomicOrdering::SeqCst), 0);
}

#[test]
fn only_an_explicit_download_prepares_a_package_and_never_installs_it() {
    let server = RepositoryServer::start();
    let (_temp, paths) = storage();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        assert_eq!(server.payload_requests.load(AtomicOrdering::SeqCst), 0);
        client.download(&paths, &mut state, ID).await.unwrap();
    });
    let entry = &state.extensions[ID];
    let pending = paths.pending_dir(ID, "1.1.1");
    assert_eq!(entry.pending_version.as_deref(), Some("1.1.1"));
    assert_eq!(fs::read(pending.join("manifest.json")).unwrap(), MANIFEST);
    assert_eq!(fs::read(pending.join("content.js")).unwrap(), SCRIPT);
    assert_eq!(hash_directory(&pending).unwrap(), entry.pending_hashes);
    assert!(entry.active_version.is_none());
    assert!(!paths.extension_dir(ID).exists());
    assert_eq!(server.file_listing_requests.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(server.payload_requests.load(AtomicOrdering::SeqCst), 1);
}

#[test]
fn failed_explicit_download_keeps_the_catalog_and_can_be_retried_without_sync() {
    let server = RepositoryServer::start();
    server.fail_download.store(true, AtomicOrdering::SeqCst);
    let (_temp, paths) = storage();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        assert!(server.fail_download.load(AtomicOrdering::SeqCst));
        let error = client.download(&paths, &mut state, ID).await.unwrap_err();
        assert!(error.contains("503"));
        assert!(state.extensions[ID].problem.is_none());
        assert!(state.extensions[ID].pending_version.is_none());
        assert!(state.extensions[ID].pending_hashes.is_empty());
        assert_eq!(fs::read_dir(&paths.staging).unwrap().count(), 0);
        client.download(&paths, &mut state, ID).await.unwrap();
    });
    assert_eq!(server.directory_requests.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(server.payload_requests.load(AtomicOrdering::SeqCst), 2);
    assert!(paths.pending_dir(ID, "1.1.1").join("content.js").is_file());
    assert!(!paths.extension_dir(ID).exists());
}

#[test]
fn sync_and_download_leave_existing_manual_edits_and_unrelated_extensions_untouched() {
    let server = RepositoryServer::start();
    let (_temp, paths) = storage();
    let active = paths.extension_dir(ID);
    fs::create_dir(&active).unwrap();
    fs::write(active.join("manifest.json"), MANIFEST).unwrap();
    fs::write(active.join("content.js"), "manual edits").unwrap();
    let unrelated = paths.root.join("OtherExtension");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("keep.txt"), "leave this alone").unwrap();
    let before = hash_directory(&active).unwrap();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        client.download(&paths, &mut state, ID).await.unwrap();
    });
    assert!(state.extensions[ID].problem.is_none());
    assert_eq!(hash_directory(&active).unwrap(), before);
    assert_eq!(
        fs::read_to_string(unrelated.join("keep.txt")).unwrap(),
        "leave this alone"
    );
    assert_eq!(
        fs::read(paths.pending_dir(ID, "1.1.1").join("content.js")).unwrap(),
        SCRIPT
    );
}

#[test]
fn each_sync_reads_the_current_manifest_version_without_fetching_extension_payloads() {
    let server = RepositoryServer::start();
    let (_temp, paths) = storage();
    let mut state = RepositoryState::default();
    let client = server.client();
    runtime().block_on(async {
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
        assert_eq!(state.extensions[ID].remote_version, "1.1.1");
        *server.manifest.lock().unwrap() =
            br#"{"manifest_version":3,"name":"Sample Extension","version":"1.2.0"}"#.to_vec();
        client
            .sync(&paths, &mut state, &ChromeState::default())
            .await
            .unwrap();
    });
    assert_eq!(state.extensions[ID].remote_version, "1.2.0");
    assert!(state.extensions[ID].pending_version.is_none());
    assert_eq!(server.file_listing_requests.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(server.payload_requests.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(server.unexpected_requests.load(AtomicOrdering::SeqCst), 0);
}
