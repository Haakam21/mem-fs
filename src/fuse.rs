use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request,
};

use crate::db::Db;
#[cfg(feature = "search")]
use crate::embeddings::Embedder;
use crate::path;
use crate::{db, queries};

const FILE_INODE_BASE: u64 = 1_000_000;
const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(0);
const BLOCK_SIZE: u32 = 512;

pub struct MemfsFs {
    conn: turso::Connection,
    mount_point: String,
    runtime: tokio::runtime::Runtime,

    next_dir_ino: AtomicU64,
    ino_to_path: RwLock<HashMap<u64, String>>,
    path_to_ino: RwLock<HashMap<String, u64>>,

    next_fh: AtomicU64,
    write_buffers: RwLock<HashMap<u64, Vec<u8>>>,
    read_cache: RwLock<HashMap<u64, Vec<u8>>>,
    db: Arc<Db>,
    #[cfg(feature = "search")]
    embedder: Option<Embedder>,

    uid: u32,
    gid: u32,
}

impl MemfsFs {
    fn alloc_dir_ino(&self, path: &str) -> u64 {
        if let Some(&ino) = self.path_to_ino.read().unwrap().get(path) {
            return ino;
        }
        let ino = self.next_dir_ino.fetch_add(1, Ordering::Relaxed);
        self.ino_to_path.write().unwrap().insert(ino, path.to_string());
        self.path_to_ino.write().unwrap().insert(path.to_string(), ino);
        ino
    }

    fn dir_path(&self, ino: u64) -> Option<String> {
        if ino == ROOT_INO {
            return Some(self.mount_point.clone());
        }
        self.ino_to_path.read().unwrap().get(&ino).cloned()
    }

    fn memory_id(ino: u64) -> i64 {
        (ino - FILE_INODE_BASE) as i64
    }

    fn file_ino(memory_id: i64) -> u64 {
        FILE_INODE_BASE + memory_id as u64
    }

    fn dir_attr(&self, ino: u64) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn file_attr(&self, ino: u64, size: u64, mtime: SystemTime, crtime: SystemTime) -> FileAttr {
        FileAttr {
            ino,
            size,
            blocks: (size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime,
            kind: FileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn parse_time(rfc3339: &str) -> SystemTime {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .map(|dt| UNIX_EPOCH + Duration::from_secs(dt.timestamp() as u64))
            .unwrap_or(SystemTime::now())
    }

    fn memory_file_attr(&self, mem: &queries::Memory) -> FileAttr {
        let ino = Self::file_ino(mem.id);
        let size = mem.content.len() as u64;
        let mtime = Self::parse_time(&mem.updated_at);
        let crtime = Self::parse_time(&mem.created_at);
        self.file_attr(ino, size, mtime, crtime)
    }

    /// Load a memory by inode and return its FileAttr, or None if not found.
    fn load_file_attr(&self, ino: u64) -> Option<FileAttr> {
        let id = Self::memory_id(ino);
        let conn = &self.conn;
        let rt = &self.runtime;
        match rt.block_on(async { queries::get_memory_by_id(conn, id).await }) {
            Ok(Some(mem)) => Some(self.memory_file_attr(&mem)),
            _ => None,
        }
    }
}

impl Filesystem for MemfsFs {
    fn init(
        &mut self,
        _req: &Request,
        _config: &mut fuser::KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        Ok(())
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if ino < FILE_INODE_BASE {
            if self.dir_path(ino).is_some() {
                reply.attr(&TTL, &self.dir_attr(ino));
            } else {
                reply.error(libc::ENOENT);
            }
        } else if let Some(attr) = self.load_file_attr(ino) {
            reply.attr(&TTL, &attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let parent_path = match self.dir_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let parsed = match path::parse(&parent_path, &self.mount_point) {
            Ok(p) => p,
            Err(_) => {
                reply.error(libc::EIO);
                return;
            }
        };

        let conn = &self.conn;
        let rt = &self.runtime;

        if parsed.is_root() {
            // At root: name must be a facet
            match rt.block_on(async { queries::facet_exists(conn, name_str).await }) {
                Ok(true) => {
                    let child_path = format!("{}/{}", parent_path, name_str);
                    let ino = self.alloc_dir_ino(&child_path);
                    reply.entry(&TTL, &self.dir_attr(ino), 0);
                }
                _ => reply.error(libc::ENOENT),
            }
        } else if parsed.is_facet_level() {
            // At facet level: name could be a value (dir) or a memory file
            let facet = parsed.trailing_facet.as_ref().unwrap().clone();

            // Check value first (directories win)
            if let Ok(true) =
                rt.block_on(async { queries::value_exists(conn, &facet, name_str).await })
            {
                let child_path = format!("{}/{}", parent_path, name_str);
                let ino = self.alloc_dir_ino(&child_path);
                reply.entry(&TTL, &self.dir_attr(ino), 0);
                return;
            }

            // Check memory file tagged under this facet
            let filters = parsed.filters.clone();
            match rt.block_on(async {
                queries::get_memory_by_facet(conn, name_str, &facet, &filters).await
            }) {
                Ok(Some(m)) => reply.entry(&TTL, &self.memory_file_attr(&m), 0),
                _ => reply.error(libc::ENOENT),
            }
        } else {
            // At value level: could be remaining facet (dir) or memory file
            let filtered: std::collections::HashSet<String> =
                parsed.filters.iter().map(|f| f.facet.clone()).collect();

            // Check remaining facet first (directories win over files)
            if !filtered.contains(name_str) {
                if let Ok(true) =
                    rt.block_on(async { queries::facet_exists(conn, name_str).await })
                {
                    let child_path = format!("{}/{}", parent_path, name_str);
                    let ino = self.alloc_dir_ino(&child_path);
                    reply.entry(&TTL, &self.dir_attr(ino), 0);
                    return;
                }
            }

            // Check memory file
            let filters = parsed.filters.clone();
            match rt.block_on(async { queries::get_memory(conn, name_str, &filters).await }) {
                Ok(Some(m)) => reply.entry(&TTL, &self.memory_file_attr(&m), 0),
                _ => reply.error(libc::ENOENT),
            }
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let path = match self.dir_path(ino) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let parsed = match path::parse(&path, &self.mount_point) {
            Ok(p) => p,
            Err(_) => {
                reply.error(libc::EIO);
                return;
            }
        };

        let conn = &self.conn;
        let rt = &self.runtime;

        // Query entries based on path level
        let items: Vec<(String, bool, Option<i64>)> = match rt.block_on(async {
            let mut items: Vec<(String, bool, Option<i64>)> = Vec::new();
            if parsed.is_root() {
                for f in queries::list_facets(conn).await? {
                    items.push((f, true, None));
                }
            } else if parsed.is_facet_level() {
                let facet = parsed.trailing_facet.as_ref().unwrap();
                let values = queries::list_values(conn, facet, &parsed.filters).await?;
                let value_set: std::collections::HashSet<&str> =
                    values.iter().map(|v| v.as_str()).collect();
                for v in &values {
                    items.push((v.clone(), true, None));
                }
                // Show files at facet-level, excluding any whose name
                // collides with a value directory (directories win)
                for m in queries::list_memory_stubs_by_facet(conn, facet, &parsed.filters).await? {
                    if !value_set.contains(m.filename.as_str()) {
                        items.push((m.filename, false, Some(m.id)));
                    }
                }
            } else {
                for f in queries::remaining_facets(conn, &parsed.filters).await? {
                    items.push((f, true, None));
                }
                for m in queries::list_memory_stubs(conn, &parsed.filters).await? {
                    items.push((m.filename, false, Some(m.id)));
                }
            }
            Ok::<_, anyhow::Error>(items)
        }) {
            Ok(items) => items,
            Err(_) => {
                reply.error(libc::EIO);
                return;
            }
        };

        // Build full entry list with . and ..
        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ino, FileType::Directory, ".".to_string()),
            (ino, FileType::Directory, "..".to_string()),
        ];

        for (name, is_dir, mem_id) in &items {
            let child_ino = if *is_dir {
                let child_path = format!("{}/{}", path, name);
                self.alloc_dir_ino(&child_path)
            } else {
                Self::file_ino(mem_id.unwrap())
            };
            let kind = if *is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            entries.push((child_ino, kind, name.clone()));
        }

        let skip = if offset >= 0 { offset as usize } else { 0 };
        for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(skip) {
            if reply.add(*child_ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn opendir(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        if ino < FILE_INODE_BASE && self.dir_path(ino).is_some() {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOTDIR);
        }
    }

    fn releasedir(&mut self, _req: &Request, _ino: u64, _fh: u64, _flags: i32, reply: ReplyEmpty) {
        reply.ok();
    }

    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        if ino < FILE_INODE_BASE {
            reply.error(libc::EISDIR);
            return;
        }

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);

        // Set up write buffer if opening for write.
        // Always start empty — truncation is handled by setattr(size=0) which
        // the kernel calls separately, and content is flushed in release().
        if flags & (libc::O_WRONLY | libc::O_RDWR) != 0 {
            self.write_buffers.write().unwrap().insert(fh, Vec::new());
        }

        reply.opened(fh, 0);
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if ino < FILE_INODE_BASE {
            reply.error(libc::EISDIR);
            return;
        }

        // Populate cache on first read for this file handle
        if !self.read_cache.read().unwrap().contains_key(&fh) {
            let id = Self::memory_id(ino);
            let conn = &self.conn;
            let rt = &self.runtime;
            match rt.block_on(async { queries::get_memory_by_id(conn, id).await }) {
                Ok(Some(mem)) => {
                    self.read_cache
                        .write()
                        .unwrap()
                        .insert(fh, mem.content.into_bytes());
                }
                _ => {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        }

        let cache = self.read_cache.read().unwrap();
        let data = &cache[&fh];
        let start = if offset >= 0 { offset as usize } else { 0 };
        if start >= data.len() {
            reply.data(&[]);
        } else {
            let end = (start + size as usize).min(data.len());
            reply.data(&data[start..end]);
        }
    }

    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let parent_path = match self.dir_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let filename = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let conn = &self.conn;
        let rt = &self.runtime;
        let mp = &self.mount_point;
        let result = rt.block_on(async {
            let parsed = path::parse(&parent_path, mp)?;

            // Build the tag set. At facet-level (e.g. /memories/people/),
            // auto-tag with facet:filename_stem so the file is properly
            // categorized (writing /memories/people/haakam.md tags with people:haakam).
            // Skip auto-tagging for temp files (e.g. .tmp.12345) — they'll be
            // renamed to the final name, which triggers proper tagging.
            let is_temp = filename.contains(".tmp.");
            let mut tags = parsed.filters.clone();
            if let Some(ref facet) = parsed.trailing_facet {
                if !is_temp {
                    let stem = std::path::Path::new(filename)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(filename);
                    tags.push(path::Filter {
                        facet: facet.clone(),
                        value: stem.to_string(),
                    });
                }
            }

            for f in &tags {
                queries::create_facet(conn, &f.facet).await?;
                queries::ensure_value(conn, &f.facet, &f.value).await?;
            }

            queries::create_memory(conn, filename, "", &tags).await
        });

        match result {
            Ok(id) => {
                let ino = Self::file_ino(id);
                let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
                self.write_buffers.write().unwrap().insert(fh, Vec::new());
                let now = SystemTime::now();
                let attr = self.file_attr(ino, 0, now, now);
                let db = self.db.clone();
                self.runtime.spawn(async move { db.push().await });
                reply.created(&TTL, &attr, 0, fh, 0);
            }
            Err(_) => reply.error(libc::EIO),
        }
    }

    fn write(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let mut buffers = self.write_buffers.write().unwrap();
        if let Some(buf) = buffers.get_mut(&fh) {
            let off = if offset >= 0 { offset as usize } else { 0 };
            if off + data.len() > buf.len() {
                buf.resize(off + data.len(), 0);
            }
            buf[off..off + data.len()].copy_from_slice(data);
            reply.written(data.len() as u32);
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn release(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let buffer = self.write_buffers.write().unwrap().remove(&fh);
        self.read_cache.write().unwrap().remove(&fh);
        if let Some(data) = buffer {
            if ino >= FILE_INODE_BASE {
                let id = Self::memory_id(ino);
                let content = String::from_utf8_lossy(&data);
                let conn = &self.conn;
                let rt = &self.runtime;
                match rt.block_on(async {
                    queries::update_memory_content(conn, id, &content).await
                }) {
                    Ok(()) => {
                        #[cfg(feature = "search")]
                        if !content.is_empty() {
                            if let Some(ref embedder) = self.embedder {
                                if let Ok(emb) = embedder.embed(&content) {
                                    let bytes = Embedder::serialize_embedding(&emb);
                                    let _ = rt.block_on(async {
                                        queries::upsert_embedding(
                                            conn,
                                            id,
                                            &bytes,
                                            embedder.model_version(),
                                        )
                                        .await
                                    });
                                }
                            }
                        }
                        // Push to cloud in background (fire-and-forget)
                        let db = self.db.clone();
                        rt.spawn(async move { db.push().await });
                        reply.ok()
                    }
                    Err(_) => reply.error(libc::EIO),
                }
                return;
            }
        }
        reply.ok();
    }

    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgflags: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // Handle truncation
        if let Some(new_size) = size {
            if ino >= FILE_INODE_BASE && new_size == 0 {
                let id = Self::memory_id(ino);
                let conn = &self.conn;
                let rt = &self.runtime;
                if rt
                    .block_on(async {
                        queries::update_memory_content(conn, id, "").await?;
                        #[cfg(feature = "search")]
                        queries::delete_embedding(conn, id).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                    .is_err()
                {
                    reply.error(libc::EIO);
                    return;
                }
            }
        }

        // Return current attributes
        if ino < FILE_INODE_BASE {
            if self.dir_path(ino).is_some() {
                reply.attr(&TTL, &self.dir_attr(ino));
            } else {
                reply.error(libc::ENOENT);
            }
        } else if let Some(mut attr) = self.load_file_attr(ino) {
            if size == Some(0) {
                attr.size = 0;
            }
            reply.attr(&TTL, &attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let parent_path = match self.dir_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let child_path = format!("{}/{}", parent_path, name_str);
        let conn = &self.conn;
        let rt = &self.runtime;
        let mp = &self.mount_point;
        let result = rt.block_on(async {
            let parsed = path::parse(&child_path, mp)?;
            if parsed.is_facet_level() {
                queries::create_facet(conn, parsed.trailing_facet.as_ref().unwrap()).await?;
            } else if !parsed.filters.is_empty() {
                let last = parsed.filters.last().unwrap();
                queries::create_facet(conn, &last.facet).await?;
                queries::ensure_value(conn, &last.facet, &last.value).await?;
            }
            Ok::<_, anyhow::Error>(())
        });

        match result {
            Ok(()) => {
                let ino = self.alloc_dir_ino(&child_path);
                let db = self.db.clone();
                self.runtime.spawn(async move { db.push().await });
                reply.entry(&TTL, &self.dir_attr(ino), 0);
            }
            Err(_) => reply.error(libc::EIO),
        }
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.dir_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let conn = &self.conn;
        let rt = &self.runtime;
        let mp = &self.mount_point;
        let result = rt.block_on(async {
            let parsed = path::parse(&parent_path, mp)?;
            match queries::get_memory(conn, name_str, &parsed.filters).await? {
                Some(m) => {
                    queries::delete_memory(conn, m.id).await?;
                    Ok(())
                }
                None => Err(anyhow::anyhow!("not found")),
            }
        });

        match result {
            Ok(()) => {
                let db = self.db.clone();
                self.runtime.spawn(async move { db.push().await });
                reply.ok()
            }
            Err(_) => reply.error(libc::ENOENT),
        }
    }

    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.dir_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let child_path = format!("{}/{}", parent_path, name_str);
        let conn = &self.conn;
        let rt = &self.runtime;
        let mp = &self.mount_point;
        let result = rt.block_on(async {
            let parsed = path::parse(&child_path, mp)?;
            if let Some(last) = parsed.filters.last() {
                queries::untag_all(conn, &last.facet, &last.value).await?;
            } else if parsed.is_facet_level() {
                queries::delete_facet(conn, parsed.trailing_facet.as_ref().unwrap()).await?;
            }
            Ok::<_, anyhow::Error>(())
        });

        match result {
            Ok(()) => {
                let db = self.db.clone();
                self.runtime.spawn(async move { db.push().await });
                reply.ok()
            }
            Err(_) => reply.error(libc::EIO),
        }
    }

    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let src_parent = match self.dir_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let dst_parent = match self.dir_path(newparent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let src_name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };
        let dst_name = match newname.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let conn = &self.conn;
        let rt = &self.runtime;
        let mp = &self.mount_point;
        let result = rt.block_on(async {
            let src_parsed = path::parse(&src_parent, mp)?;
            let dst_parsed = path::parse(&dst_parent, mp)?;

            let mem = queries::get_memory(conn, src_name, &src_parsed.filters)
                .await?
                .ok_or_else(|| anyhow::anyhow!("not found"))?;

            // Compute tag diff
            let src_set: std::collections::HashSet<(String, String)> = src_parsed
                .filters
                .iter()
                .map(|f| (f.facet.clone(), f.value.clone()))
                .collect();
            let dst_set: std::collections::HashSet<(String, String)> = dst_parsed
                .filters
                .iter()
                .map(|f| (f.facet.clone(), f.value.clone()))
                .collect();

            for (facet, value) in src_set.difference(&dst_set) {
                queries::remove_tag(conn, mem.id, facet, value).await?;
            }
            for (facet, value) in dst_set.difference(&src_set) {
                queries::add_tag(conn, mem.id, facet, value).await?;
            }

            if src_name != dst_name {
                queries::rename_memory(conn, mem.id, dst_name).await?;

                // At facet-level, update the auto-tag: remove old stem, add new stem
                if let Some(ref facet) = dst_parsed.trailing_facet {
                    let old_stem = std::path::Path::new(src_name)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(src_name);
                    let new_stem = std::path::Path::new(dst_name)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(dst_name);
                    queries::remove_tag(conn, mem.id, facet, old_stem).await?;
                    queries::create_facet(conn, facet).await?;
                    queries::ensure_value(conn, facet, new_stem).await?;
                    queries::add_tag(conn, mem.id, facet, new_stem).await?;
                }
            }

            Ok::<_, anyhow::Error>(())
        });

        match result {
            Ok(()) => {
                let db = self.db.clone();
                self.runtime.spawn(async move { db.push().await });
                reply.ok()
            }
            Err(_) => reply.error(libc::EIO),
        }
    }
}

// --- Public API ---

pub fn mount(
    db_path: &str,
    virtual_mount: &str,
    fuse_mountpoint: &str,
    foreground: bool,
) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;

    let settings = crate::settings::load(db_path);
    let database = runtime.block_on(db::open(db_path, &settings))?;
    let db = Arc::new(database);
    let conn = runtime.block_on(db.connect())?;
    runtime.block_on(db::migrate(&conn))?;

    #[cfg(feature = "search")]
    let embedder = Embedder::try_load().unwrap_or(None);

    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut ino_to_path = HashMap::new();
    let mut path_to_ino = HashMap::new();
    ino_to_path.insert(ROOT_INO, virtual_mount.to_string());
    path_to_ino.insert(virtual_mount.to_string(), ROOT_INO);

    let fs = MemfsFs {
        conn,
        mount_point: virtual_mount.to_string(),
        runtime,
        next_dir_ino: AtomicU64::new(2),
        ino_to_path: RwLock::new(ino_to_path),
        path_to_ino: RwLock::new(path_to_ino),
        next_fh: AtomicU64::new(1),
        write_buffers: RwLock::new(HashMap::new()),
        read_cache: RwLock::new(HashMap::new()),
        db,
        #[cfg(feature = "search")]
        embedder,
        uid,
        gid,
    };

    std::fs::create_dir_all(fuse_mountpoint)?;

    let options = vec![
        MountOption::FSName("memfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::RW,
    ];

    let _ = foreground; // Flag accepted for compatibility; caller backgrounds with &
    eprintln!(
        "memfs: mounting at {} (Ctrl+C or `memfs unmount {}` to stop)",
        fuse_mountpoint, fuse_mountpoint
    );

    fuser::mount2(fs, fuse_mountpoint, &options)?;
    Ok(())
}

pub fn unmount(mountpoint: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("umount")
            .arg(mountpoint)
            .status()?
    } else {
        std::process::Command::new("fusermount")
            .arg("-u")
            .arg(mountpoint)
            .status()?
    };

    if status.success() {
        eprintln!("memfs: unmounted {}", mountpoint);
        Ok(())
    } else {
        anyhow::bail!("memfs: failed to unmount {}", mountpoint)
    }
}
