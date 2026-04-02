use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request,
};

use crate::path;
use crate::{db, queries};

const FILE_INODE_BASE: u64 = 1_000_000;
const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(1);
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
}

impl Filesystem for MemfsFs {
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if ino < FILE_INODE_BASE {
            if self.dir_path(ino).is_some() {
                reply.attr(&TTL, &self.dir_attr(ino));
            } else {
                reply.error(libc::ENOENT);
            }
            return;
        }

        let id = Self::memory_id(ino);
        let conn = &self.conn;
        let rt = &self.runtime;
        let result = rt.block_on(async { queries::get_memory_by_id(conn, id).await });
        match result {
            Ok(Some(mem)) => {
                let size = mem.content.len() as u64;
                let mtime = Self::parse_time(&mem.updated_at);
                let crtime = Self::parse_time(&mem.created_at);
                reply.attr(&TTL, &self.file_attr(ino, size, mtime, crtime));
            }
            _ => reply.error(libc::ENOENT),
        }
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::ENOENT);
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
            // At facet level: name must be a value
            let facet = parsed.trailing_facet.as_ref().unwrap().clone();
            match rt.block_on(async { queries::value_exists(conn, &facet, name_str).await }) {
                Ok(true) => {
                    let child_path = format!("{}/{}", parent_path, name_str);
                    let ino = self.alloc_dir_ino(&child_path);
                    reply.entry(&TTL, &self.dir_attr(ino), 0);
                }
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
                Ok(Some(m)) => {
                    let ino = Self::file_ino(m.id);
                    let size = m.content.len() as u64;
                    let mtime = Self::parse_time(&m.updated_at);
                    let crtime = Self::parse_time(&m.created_at);
                    reply.entry(&TTL, &self.file_attr(ino, size, mtime, crtime), 0);
                }
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
                for v in queries::list_values(conn, facet, &parsed.filters).await? {
                    items.push((v, true, None));
                }
            } else {
                for f in queries::remaining_facets(conn, &parsed.filters).await? {
                    items.push((f, true, None));
                }
                for m in queries::list_memories(conn, &parsed.filters).await? {
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

        for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
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
        _fh: u64,
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

        let id = Self::memory_id(ino);
        let conn = &self.conn;
        let rt = &self.runtime;
        match rt.block_on(async { queries::get_memory_by_id(conn, id).await }) {
            Ok(Some(mem)) => {
                let data = mem.content.as_bytes();
                let start = offset as usize;
                if start >= data.len() {
                    reply.data(&[]);
                } else {
                    let end = (start + size as usize).min(data.len());
                    reply.data(&data[start..end]);
                }
            }
            _ => reply.error(libc::ENOENT),
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
            // Ensure facets/values exist
            for f in &parsed.filters {
                queries::create_facet(conn, &f.facet).await?;
                queries::ensure_value(conn, &f.facet, &f.value).await?;
            }
            queries::create_memory(conn, filename, "", &parsed.filters).await
        });

        match result {
            Ok(new_id) => {
                let ino = Self::file_ino(new_id);
                let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
                self.write_buffers.write().unwrap().insert(fh, Vec::new());
                let now = SystemTime::now();
                let attr = self.file_attr(ino, 0, now, now);
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
            let off = offset as usize;
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
        if let Some(data) = buffer {
            if ino >= FILE_INODE_BASE {
                let id = Self::memory_id(ino);
                let content = String::from_utf8_lossy(&data);
                let conn = &self.conn;
                let rt = &self.runtime;
                let _ = rt.block_on(async {
                    queries::update_memory_content(conn, id, &content).await
                });
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
                let _ = rt.block_on(async {
                    queries::update_memory_content(conn, id, "").await
                });
            }
        }

        // Return current attributes
        if ino < FILE_INODE_BASE {
            if self.dir_path(ino).is_some() {
                reply.attr(&TTL, &self.dir_attr(ino));
            } else {
                reply.error(libc::ENOENT);
            }
        } else {
            let id = Self::memory_id(ino);
            let conn = &self.conn;
            let rt = &self.runtime;
            match rt.block_on(async { queries::get_memory_by_id(conn, id).await }) {
                Ok(Some(mem)) => {
                    let content_size = if size == Some(0) {
                        0u64
                    } else {
                        mem.content.len() as u64
                    };
                    let mtime = Self::parse_time(&mem.updated_at);
                    let crtime = Self::parse_time(&mem.created_at);
                    reply.attr(&TTL, &self.file_attr(ino, content_size, mtime, crtime));
                }
                _ => reply.error(libc::ENOENT),
            }
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
            Ok(()) => reply.ok(),
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
            Ok(()) => reply.ok(),
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
            }

            Ok::<_, anyhow::Error>(())
        });

        match result {
            Ok(()) => reply.ok(),
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

    let database = runtime.block_on(db::open(db_path))?;
    let conn = database.connect()?;
    runtime.block_on(db::migrate(&conn))?;

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
        uid,
        gid,
    };

    std::fs::create_dir_all(fuse_mountpoint)?;

    let options = vec![
        MountOption::FSName("memfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::RW,
    ];

    if !foreground {
        // AutoUnmount handles cleanup when process exits
        // For true background, user can run with & or via init system
        eprintln!(
            "memfs: mounting at {} (use memfs unmount {} to stop)",
            fuse_mountpoint, fuse_mountpoint
        );
    } else {
        eprintln!(
            "memfs: mounting at {} (press Ctrl+C to unmount)",
            fuse_mountpoint
        );
    }

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
