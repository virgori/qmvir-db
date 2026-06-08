/*
 * Page Management - Memory-mapped page storage
 */

use memmap2::{MmapMut, MmapOptions};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// Page header (64 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PageHeader {
    pub page_id: u32,
    pub page_type: u8,
    pub flags: u8,
    pub free_space_offset: u16,
    pub free_space_end: u16,
    pub item_count: u16,
    pub checksum: u32,
    pub lsn: u64,
    pub prev_page: u32,
    pub next_page: u32,
    pub _reserved: [u8; 32],
}

impl PageHeader {
    pub const SIZE: usize = 64;

    pub fn new(page_id: u32, page_type: PageType, page_size: usize) -> Self {
        Self {
            page_id,
            page_type: page_type as u8,
            flags: 0,
            free_space_offset: Self::SIZE as u16,
            free_space_end: page_size as u16,
            item_count: 0,
            checksum: 0,
            lsn: 0,
            prev_page: 0,
            next_page: 0,
            _reserved: [0; 32],
        }
    }

    pub fn from_bytes(buf: &[u8]) -> Self {
        assert!(buf.len() >= Self::SIZE);
        unsafe { std::ptr::read(buf.as_ptr() as *const Self) }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        unsafe { std::mem::transmute(*self) }
    }
}

/// Page types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    Empty = 0,
    Data = 1,
    Index = 2,
    Overflow = 3,
    Free = 4,
}

/// A single page in storage
pub struct Page {
    header: PageHeader,
    data: Vec<u8>,
    is_dirty: bool,
}

impl Page {
    pub fn new(page_id: u32, page_type: PageType, page_size: usize) -> Self {
        let header = PageHeader::new(page_id, page_type, page_size);
        let mut data = vec![0u8; page_size];

        // Write header to data
        data[..PageHeader::SIZE].copy_from_slice(&header.to_bytes());

        Self {
            header,
            data,
            is_dirty: true,
        }
    }

    pub fn from_bytes(data: Vec<u8>) -> Self {
        let header = PageHeader::from_bytes(&data);
        Self {
            header,
            data,
            is_dirty: false,
        }
    }

    pub fn page_id(&self) -> u32 {
        self.header.page_id
    }

    pub fn is_dirty(&self) -> bool {
        self.is_dirty
    }

    pub fn mark_dirty(&mut self) {
        self.is_dirty = true;
    }

    pub fn mark_clean(&mut self) {
        self.is_dirty = false;
    }

    /// Get free space
    pub fn free_space(&self) -> usize {
        (self.header.free_space_end - self.header.free_space_offset) as usize
    }

    /// Insert data into page
    pub fn insert(&mut self, data: &[u8]) -> Option<u16> {
        let data_len = data.len();
        if self.free_space() < data_len + 4 {
            return None;
        }

        // Insert item pointer at free_space_offset
        let item_offset = self.header.free_space_end - data_len as u16;
        let ptr_offset = self.header.free_space_offset as usize;

        // Write length + offset as item pointer
        self.data[ptr_offset..ptr_offset + 2].copy_from_slice(&item_offset.to_le_bytes());
        self.data[ptr_offset + 2..ptr_offset + 4].copy_from_slice(&(data_len as u16).to_le_bytes());

        // Write data
        self.data[item_offset as usize..item_offset as usize + data_len].copy_from_slice(data);

        // Update header
        self.header.free_space_offset += 4;
        self.header.free_space_end = item_offset;
        self.header.item_count += 1;

        self.is_dirty = true;
        Some(self.header.item_count - 1)
    }

    /// Get data by item index
    pub fn get(&self, item_idx: u16) -> Option<&[u8]> {
        if item_idx >= self.header.item_count {
            return None;
        }

        let ptr_offset = PageHeader::SIZE + (item_idx as usize * 4);
        let item_offset =
            u16::from_le_bytes([self.data[ptr_offset], self.data[ptr_offset + 1]]) as usize;
        let item_len =
            u16::from_le_bytes([self.data[ptr_offset + 2], self.data[ptr_offset + 3]]) as usize;

        Some(&self.data[item_offset..item_offset + item_len])
    }

    /// Get raw data
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Update header in data
    pub fn sync_header(&mut self) {
        self.data[..PageHeader::SIZE].copy_from_slice(&self.header.to_bytes());
    }
}

/// Page file manager
pub struct PageFile {
    file: File,
    mmap: Option<MmapMut>,
    page_size: usize,
    page_count: AtomicU32,
}

impl PageFile {
    pub fn open(path: &PathBuf, page_size: usize) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        let file_size = file.metadata()?.len() as usize;
        let page_count = if file_size > 0 {
            file_size / page_size
        } else {
            0
        };

        Ok(Self {
            file,
            mmap: None,
            page_size,
            page_count: AtomicU32::new(page_count as u32),
        })
    }

    /// Allocate a new page
    pub fn allocate_page(&mut self) -> io::Result<u32> {
        let page_id = self.page_count.fetch_add(1, Ordering::SeqCst);

        // Extend file
        let new_size = (page_id as usize + 1) * self.page_size;
        self.file.set_len(new_size as u64)?;

        // Invalidate mmap
        self.mmap = None;

        Ok(page_id)
    }

    /// Read a page
    pub fn read_page(&mut self, page_id: u32) -> io::Result<Page> {
        let offset = page_id as usize * self.page_size;

        // Ensure mmap is set up
        if self.mmap.is_none() {
            unsafe {
                self.mmap = Some(MmapOptions::new().map_mut(&self.file)?);
            }
        }

        let mmap = self.mmap.as_ref().unwrap();
        if offset + self.page_size > mmap.len() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Page not found"));
        }

        let data = mmap[offset..offset + self.page_size].to_vec();
        Ok(Page::from_bytes(data))
    }

    /// Write a page
    pub fn write_page(&mut self, page: &mut Page) -> io::Result<()> {
        let page_id = page.page_id();
        let offset = page_id as usize * self.page_size;

        page.sync_header();

        // Ensure mmap is set up
        if self.mmap.is_none() {
            unsafe {
                self.mmap = Some(MmapOptions::new().map_mut(&self.file)?);
            }
        }

        let mmap = self.mmap.as_mut().unwrap();
        if offset + self.page_size > mmap.len() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Page not found"));
        }

        mmap[offset..offset + self.page_size].copy_from_slice(page.as_bytes());
        mmap.flush_range(offset, self.page_size)?;

        page.mark_clean();
        Ok(())
    }

    /// Sync all pages to disk
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_page_insert_get() {
        let mut page = Page::new(0, PageType::Data, 8192);

        let data1 = b"Hello, World!";
        let idx1 = page.insert(data1).unwrap();

        let data2 = b"Another record";
        let idx2 = page.insert(data2).unwrap();

        assert_eq!(page.get(idx1).unwrap(), data1);
        assert_eq!(page.get(idx2).unwrap(), data2);
    }

    #[test]
    fn test_page_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");

        let mut pf = PageFile::open(&path, 8192).unwrap();

        // Allocate and write page
        let page_id = pf.allocate_page().unwrap();
        let mut page = Page::new(page_id, PageType::Data, 8192);
        page.insert(b"test data").unwrap();
        pf.write_page(&mut page).unwrap();

        // Read back
        let page2 = pf.read_page(page_id).unwrap();
        assert_eq!(page2.get(0).unwrap(), b"test data");
    }
}
