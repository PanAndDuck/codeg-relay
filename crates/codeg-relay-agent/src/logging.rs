use std::{
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct RotatingLog(Arc<Mutex<Inner>>);
struct Inner {
    file: std::fs::File,
    dir: PathBuf,
    size: u64,
    max_bytes: u64,
    backups: usize,
}
impl RotatingLog {
    pub fn open(dir: &Path, max_bytes: u64, backups: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            max_bytes > 0 && (1..=20).contains(&backups),
            "invalid log rotation limits"
        );
        crate::secrets::private_dir(dir)?;
        let mut file = crate::secrets::open_private(&dir.join("agent.log"), true)?;
        let size = file.seek(SeekFrom::End(0))?;
        Ok(Self(Arc::new(Mutex::new(Inner {
            file,
            dir: dir.to_owned(),
            size,
            max_bytes,
            backups,
        }))))
    }
}
impl Inner {
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let oldest = self.dir.join(format!("agent.log.{}", self.backups));
        if oldest.exists() {
            std::fs::remove_file(oldest)?;
        }
        for i in (1..self.backups).rev() {
            let from = self.dir.join(format!("agent.log.{i}"));
            if from.exists() {
                std::fs::rename(from, self.dir.join(format!("agent.log.{}", i + 1)))?;
            }
        }
        std::fs::rename(self.dir.join("agent.log"), self.dir.join("agent.log.1"))?;
        self.file = crate::secrets::open_private(&self.dir.join("agent.log"), true)
            .map_err(|_| io::Error::other("cannot rotate log"))?;
        self.size = 0;
        Ok(())
    }
}
impl Write for RotatingLog {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let len = bytes.len();
        let mut inner = self.0.lock().unwrap();
        while !bytes.is_empty() {
            if inner.size >= inner.max_bytes {
                inner.rotate()?;
            }
            let n = bytes.len().min((inner.max_bytes - inner.size) as usize);
            inner.file.write_all(&bytes[..n])?;
            inner.size += n as u64;
            bytes = &bytes[n..];
        }
        Ok(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().file.flush()
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingLog {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotates_at_size_bound_and_keeps_only_bounded_archives() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::open(dir.path(), 64, 3).unwrap();
        for _ in 0..100 {
            log.write_all(b"bounded event\n").unwrap();
        }
        log.flush().unwrap();
        drop(log);
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(files.len(), 4);
        for file in files {
            assert!(file.metadata().unwrap().len() <= 64);
        }
    }
}
