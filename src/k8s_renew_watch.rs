use std::{path::PathBuf, sync::Arc};

use crossfire::{
    Tx,
    spsc::{self, List},
};
use eyre::Result;
use inotify::{Inotify, WatchMask};
use tokio::{spawn, task::spawn_blocking};
use tracing::{debug, warn};

pub struct MountFolderWatcher {
    pub mount_folders: Arc<Vec<PathBuf>>,
}
impl MountFolderWatcher {
    pub async fn run<F>(&self, callback: F) -> Result<()>
    where
        F: AsyncFn(Option<String>) -> Result<()>,
    {
        let (tx, rx) = spsc::unbounded_async::<Option<String>>();
        let mf = self.mount_folders.clone();
        spawn(spawn_blocking(move || watch(mf.as_slice(), &tx)));

        loop {
            let msg = rx.recv().await?;
            callback(msg).await?;
        }
    }
}

fn watch(mount_folders: &[PathBuf], tx: &Tx<List<Option<String>>>) -> Result<()> {
    let mut inotify = Inotify::init()?;
    for mf in mount_folders {
        inotify.watches().add(mf, WatchMask::MOVED_TO)?;
    }

    let mut buffer = [0; 4096];
    loop {
        let events = inotify.read_events_blocking(&mut buffer)?;
        for event in events {
            debug!("{event:?}");
            if let Err(e) = tx.try_send(event.name.map(|os| os.to_string_lossy().to_string())) {
                warn!("{e:?}");
            }
        }
    }
}
