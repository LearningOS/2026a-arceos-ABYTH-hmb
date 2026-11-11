//! A RAM filesystem with rename support, used for the `/tmp` mount point.
//!
//! The upstream `axfs_ramfs` crate (0.1) does not implement `rename`, so this
//! module provides a drop-in RAM filesystem whose directory nodes support
//! renaming entries within the same directory, preserving the file contents.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use axfs_vfs::{
    VfsDirEntry, VfsError, VfsNodeAttr, VfsNodeOps, VfsNodeRef, VfsNodeType, VfsOps, VfsResult,
};
use axsync::Mutex;

/// A node stored in a directory of this filesystem.
#[derive(Clone)]
enum Node {
    File(Arc<FileNode>),
    Dir(Arc<DirNode>),
}

impl Node {
    fn node_ref(&self) -> VfsNodeRef {
        match self {
            Node::File(f) => f.clone() as VfsNodeRef,
            Node::Dir(d) => d.clone() as VfsNodeRef,
        }
    }

    fn file_type(&self) -> VfsNodeType {
        match self {
            Node::File(_) => VfsNodeType::File,
            Node::Dir(_) => VfsNodeType::Dir,
        }
    }
}

/// The file node in this filesystem.
pub struct FileNode {
    content: Mutex<Vec<u8>>,
}

impl FileNode {
    pub(super) fn new() -> Self {
        Self {
            content: Mutex::new(Vec::new()),
        }
    }
}

impl VfsNodeOps for FileNode {
    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_file(self.content.lock().len() as _, 0))
    }

    fn truncate(&self, size: u64) -> VfsResult {
        let mut content = self.content.lock();
        if size < content.len() as u64 {
            content.truncate(size as usize);
        } else {
            content.resize(size as usize, 0);
        }
        Ok(())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let content = self.content.lock();
        let start = content.len().min(offset as usize);
        let end = content.len().min(offset as usize + buf.len());
        let src = &content[start..end];
        buf[..src.len()].copy_from_slice(src);
        Ok(src.len())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> VfsResult<usize> {
        let offset = offset as usize;
        let mut content = self.content.lock();
        if offset + buf.len() > content.len() {
            content.resize(offset + buf.len(), 0);
        }
        let dst = &mut content[offset..offset + buf.len()];
        dst.copy_from_slice(&buf[..dst.len()]);
        Ok(buf.len())
    }
}

/// The directory node in this filesystem.
pub struct DirNode {
    this: Weak<DirNode>,
    parent: Mutex<Weak<dyn VfsNodeOps>>,
    children: Mutex<BTreeMap<String, Node>>,
}

impl DirNode {
    pub(super) fn new(parent: Option<Weak<dyn VfsNodeOps>>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            parent: Mutex::new(parent.unwrap_or_else(|| Weak::<Self>::new())),
            children: Mutex::new(BTreeMap::new()),
        })
    }

    pub(super) fn set_parent(&self, parent: Option<Weak<dyn VfsNodeOps>>) {
        *self.parent.lock() = parent.unwrap_or_else(|| Weak::<Self>::new());
    }

    /// Returns a string list of all entries in this directory.
    pub fn get_entries(&self) -> Vec<String> {
        self.children.lock().keys().cloned().collect()
    }

    /// Checks whether a node with the given name exists in this directory.
    pub fn exist(&self, name: &str) -> bool {
        self.children.lock().contains_key(name)
    }

    /// Creates a new node with the given name and type in this directory.
    pub fn create_node(&self, name: &str, ty: VfsNodeType) -> VfsResult {
        let mut children = self.children.lock();
        if children.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let node = match ty {
            VfsNodeType::File => Node::File(Arc::new(FileNode::new())),
            VfsNodeType::Dir => Node::Dir(DirNode::new(Some(self.this.clone()))),
            _ => return Err(VfsError::Unsupported),
        };
        children.insert(name.into(), node);
        Ok(())
    }

    /// Removes a node by the given name in this directory.
    pub fn remove_node(&self, name: &str) -> VfsResult {
        let mut children = self.children.lock();
        let node = children.get(name).ok_or(VfsError::NotFound)?;
        if let Node::Dir(dir) = node {
            if !dir.children.lock().is_empty() {
                return Err(VfsError::DirectoryNotEmpty);
            }
        }
        children.remove(name);
        Ok(())
    }

    /// Renames a node in this directory. Only renaming within the same
    /// directory is supported (no move); the file contents are preserved
    /// because the same node object is re-inserted under the new name.
    pub fn rename_node(&self, src_name: &str, dst_name: &str) -> VfsResult {
        if dst_name.is_empty() || dst_name == "." || dst_name == ".." {
            return Err(VfsError::InvalidInput);
        }
        let mut children = self.children.lock();
        let node = children.get(src_name).cloned().ok_or(VfsError::NotFound)?;
        children.remove(src_name);
        children.insert(dst_name.into(), node);
        Ok(())
    }
}

impl VfsNodeOps for DirNode {
    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_dir(4096, 0))
    }

    fn parent(&self) -> Option<VfsNodeRef> {
        self.parent.lock().upgrade()
    }

    fn lookup(self: Arc<Self>, path: &str) -> VfsResult<VfsNodeRef> {
        let (name, rest) = split_path(path);
        let node = match name {
            "" | "." => Ok(self.clone() as VfsNodeRef),
            ".." => self.parent().ok_or(VfsError::NotFound),
            _ => {
                let children = self.children.lock();
                children.get(name).map(Node::node_ref).ok_or(VfsError::NotFound)
            }
        }?;

        if let Some(rest) = rest {
            node.lookup(rest)
        } else {
            Ok(node)
        }
    }

    fn read_dir(&self, start_idx: usize, dirents: &mut [VfsDirEntry]) -> VfsResult<usize> {
        let children = self.children.lock();
        let mut children = children.iter().skip(start_idx.max(2) - 2);
        for (i, ent) in dirents.iter_mut().enumerate() {
            match i + start_idx {
                0 => *ent = VfsDirEntry::new(".", VfsNodeType::Dir),
                1 => *ent = VfsDirEntry::new("..", VfsNodeType::Dir),
                _ => {
                    if let Some((name, node)) = children.next() {
                        *ent = VfsDirEntry::new(name, node.file_type());
                    } else {
                        return Ok(i);
                    }
                }
            }
        }
        Ok(dirents.len())
    }

    fn create(&self, path: &str, ty: VfsNodeType) -> VfsResult {
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            match name {
                "" | "." => self.create(rest, ty),
                ".." => self.parent().ok_or(VfsError::NotFound)?.create(rest, ty),
                _ => {
                    let subdir = match self.children.lock().get(name) {
                        Some(Node::Dir(d)) => d.clone(),
                        _ => return Err(VfsError::NotFound),
                    };
                    subdir.create(rest, ty)
                }
            }
        } else if name.is_empty() || name == "." || name == ".." {
            Ok(()) // already exists
        } else {
            self.create_node(name, ty)
        }
    }

    fn remove(&self, path: &str) -> VfsResult {
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            match name {
                "" | "." => self.remove(rest),
                ".." => self.parent().ok_or(VfsError::NotFound)?.remove(rest),
                _ => {
                    let subdir = match self.children.lock().get(name) {
                        Some(Node::Dir(d)) => d.clone(),
                        _ => return Err(VfsError::NotFound),
                    };
                    subdir.remove(rest)
                }
            }
        } else if name.is_empty() || name == "." || name == ".." {
            Err(VfsError::InvalidInput) // remove '.' or '..'
        } else {
            self.remove_node(name)
        }
    }

    fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
        // `src_path` is relative to this directory, while `dst_path` may be
        // an absolute path (the caller passes it through unchanged). Only
        // same-directory rename is supported, so we take the last component
        // of the destination path as the new name.
        let (src_name, src_rest) = split_path(src_path);
        if let Some(rest) = src_rest {
            let subdir = match self.children.lock().get(src_name) {
                Some(Node::Dir(d)) => d.clone(),
                _ => return Err(VfsError::NotFound),
            };
            return subdir.rename(rest, dst_path);
        }
        let dst_name = dst_path
            .trim_start_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        self.rename_node(src_name, dst_name)
    }
}

fn split_path(path: &str) -> (&str, Option<&str>) {
    let trimmed_path = path.trim_start_matches('/');
    trimmed_path.find('/').map_or((trimmed_path, None), |n| {
        (&trimmed_path[..n], Some(&trimmed_path[n + 1..]))
    })
}

/// A RAM filesystem that implements [`axfs_vfs::VfsOps`] and supports rename.
pub struct RamFileSystem {
    root: Arc<DirNode>,
}

impl RamFileSystem {
    /// Create a new instance.
    pub fn new() -> Self {
        Self {
            root: DirNode::new(None),
        }
    }

    /// Returns the root directory node in [`Arc<DirNode>`](DirNode).
    pub fn root_dir_node(&self) -> Arc<DirNode> {
        self.root.clone()
    }
}

impl VfsOps for RamFileSystem {
    fn mount(&self, _path: &str, mount_point: VfsNodeRef) -> VfsResult {
        let parent = mount_point.parent().map(|p| Arc::downgrade(&p));
        self.root.set_parent(parent);
        Ok(())
    }

    fn root_dir(&self) -> VfsNodeRef {
        self.root.clone()
    }
}
