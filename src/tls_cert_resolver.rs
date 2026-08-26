use std::{
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use arc_swap::ArcSwap;
use eyre::Result;
use inotify::{Inotify, WatchMask};
use rustls::{
    crypto::aws_lc_rs::sign::any_supported_type,
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use rustls_pemfile::{certs, private_key};
use tokio::{fs, time::sleep};
use tracing::instrument;

// This is for Actix to hot-reload renewed TLS cert.
// In this application's certain case, there is only one cert.
// Hence no judgement from `client_hello.server_name()` or so.
#[derive(Debug)]
pub struct TLSCertResolver {
    certified_key: ArcSwap<CertifiedKey>,
}
impl TLSCertResolver {
    #[allow(unreachable_code)]
    #[instrument(skip_all)]
    pub async fn new(
        tls_folder: PathBuf,
        cert_file_name: &str,
        key_file_name: &str,
    ) -> Result<Arc<Self>> {
        let cert_path = tls_folder.join(cert_file_name);
        let key_path = tls_folder.join(key_file_name);
        let self_ = Arc::new(Self {
            certified_key: ArcSwap::new(load_certified_key(&cert_path, &key_path).await?.into()),
        });

        let watcher = self_.clone();
        tokio::spawn(async move {
            let mut inotify = Inotify::init()?;
            inotify.watches().add(
                tls_folder,
                WatchMask::DELETE | WatchMask::CREATE | WatchMask::MOVED_TO,
            )?;

            let mut buffer = [0; 4096];
            let c = cert_path.clone();
            let k = key_path.clone();
            loop {
                let events = inotify.read_events(&mut buffer)?;
                for event in events {
                    if let Some(name) = event.name
                        && name == "..data"
                    {
                        tracing::info!(target: "tls-cert-hot-reload", message = "TLS cert renewed");
                        watcher
                            .certified_key
                            .store(load_certified_key(&c, &k).await?.into());
                        break;
                    }
                }

                sleep(Duration::from_millis(1)).await;
            }

            Ok(()) as Result<()> // YOU SUCK!
        });

        Ok(self_)
    }
}
impl ResolvesServerCert for TLSCertResolver {
    #[instrument(skip_all)]
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.certified_key.load_full())
    }
}

async fn load_certified_key(cert_path: &Path, key_path: &Path) -> Result<CertifiedKey> {
    let cert_bytes = fs::read(cert_path).await?;
    let key_bytes = fs::read(key_path).await?;
    let mut cert_buf = BufReader::new(&cert_bytes[..]);
    let mut key_buf = BufReader::new(&key_bytes[..]);
    let certs = certs(&mut cert_buf)
        .map(|c| Ok(c?))
        .collect::<Result<Vec<_>>>()?;
    let key = private_key(&mut key_buf)?.expect("TLS key cannot be loaded");
    Ok(CertifiedKey::new(certs, any_supported_type(&key)?))
}
