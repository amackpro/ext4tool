use anyhow::{anyhow, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const SPARSE_HEADER_MAGIC: u32 = 0xED26FF3A;
const CHUNK_TYPE_RAW: u16 = 0xCAC1;
const CHUNK_TYPE_FILL: u16 = 0xCAC2;
const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;

#[derive(Debug)]
struct SparseHeader {
    magic: u32,
    major_version: u16,
    minor_version: u16,
    file_hdr_sz: u16,
    chunk_hdr_sz: u16,
    blk_sz: u32,
    total_blks: u32,
    total_chunks: u32,
    image_checksum: u32,
}

impl SparseHeader {
    fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let magic = reader.read_u32::<LittleEndian>()?;
        if magic != SPARSE_HEADER_MAGIC {
            return Err(anyhow!("Not a sparse image (magic: 0x{:X})", magic));
        }

        let major_version = reader.read_u16::<LittleEndian>()?;
        let minor_version = reader.read_u16::<LittleEndian>()?;
        let file_hdr_sz = reader.read_u16::<LittleEndian>()?;
        let chunk_hdr_sz = reader.read_u16::<LittleEndian>()?;
        let blk_sz = reader.read_u32::<LittleEndian>()?;
        let total_blks = reader.read_u32::<LittleEndian>()?;
        let total_chunks = reader.read_u32::<LittleEndian>()?;
        let image_checksum = reader.read_u32::<LittleEndian>()?;

        Ok(SparseHeader {
            magic,
            major_version,
            minor_version,
            file_hdr_sz,
            chunk_hdr_sz,
            blk_sz,
            total_blks,
            total_chunks,
            image_checksum,
        })
    }
}

#[derive(Debug)]
struct ChunkHeader {
    chunk_type: u16,
    reserved1: u16,
    chunk_sz: u32,
    total_sz: u32,
}

impl ChunkHeader {
    fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let chunk_type = reader.read_u16::<LittleEndian>()?;
        let reserved1 = reader.read_u16::<LittleEndian>()?;
        let chunk_sz = reader.read_u32::<LittleEndian>()?;
        let total_sz = reader.read_u32::<LittleEndian>()?;

        Ok(ChunkHeader {
            chunk_type,
            reserved1,
            chunk_sz,
            total_sz,
        })
    }
}

pub fn is_sparse_image<P: AsRef<Path>>(path: P) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 4 * 1024 * 1024]; // Search first 4MB
    let bytes_read = file.read(&mut buffer)?;
    buffer.truncate(bytes_read);

    // Search for sparse magic
    for i in 0..buffer.len().saturating_sub(4) {
        let magic = u32::from_le_bytes([buffer[i], buffer[i+1], buffer[i+2], buffer[i+3]]);
        if magic == SPARSE_HEADER_MAGIC {
            return Ok(true);
        }
    }

    Ok(false)
}

pub fn convert_sparse_to_raw<P: AsRef<Path>>(sparse_path: P, raw_path: P) -> Result<()> {
    let mut input = BufReader::new(File::open(sparse_path.as_ref())?);
    let mut output = BufWriter::new(File::create(raw_path.as_ref())?);

    // Search for sparse header in first 4MB
    let mut search_buffer = vec![0u8; 4 * 1024 * 1024];
    let bytes_read = input.read(&mut search_buffer)?;
    search_buffer.truncate(bytes_read);

    let mut header_offset = None;
    for i in 0..search_buffer.len().saturating_sub(4) {
        let magic = u32::from_le_bytes([
            search_buffer[i],
            search_buffer[i+1],
            search_buffer[i+2],
            search_buffer[i+3]
        ]);
        if magic == SPARSE_HEADER_MAGIC {
            header_offset = Some(i);
            break;
        }
    }

    let offset = header_offset.ok_or_else(|| anyhow!("Sparse header not found"))?;

    // Seek to header
    input.seek(SeekFrom::Start(offset as u64))?;

    // Read sparse header
    let header = SparseHeader::read_from(&mut input)?;

    println!("Sparse image info:");
    println!("  Version: {}.{}", header.major_version, header.minor_version);
    println!("  Block size: {} bytes", header.blk_sz);
    println!("  Total blocks: {}", header.total_blks);
    println!("  Total chunks: {}", header.total_chunks);

    // Skip to end of file header
    let current_pos = offset + 28; // Size of sparse header we read
    if header.file_hdr_sz as usize > 28 {
        input.seek(SeekFrom::Start((offset + header.file_hdr_sz as usize) as u64))?;
    }

    // Process chunks
    let mut blocks_written = 0u64;

    for chunk_idx in 0..header.total_chunks {
        let chunk = ChunkHeader::read_from(&mut input)?;

        match chunk.chunk_type {
            CHUNK_TYPE_RAW => {
                // Raw data - copy directly
                let data_size = (chunk.chunk_sz as u64) * (header.blk_sz as u64);
                let mut remaining = data_size;
                let mut buffer = vec![0u8; 1024 * 1024]; // 1MB buffer

                while remaining > 0 {
                    let to_read = remaining.min(buffer.len() as u64) as usize;
                    input.read_exact(&mut buffer[..to_read])?;
                    output.write_all(&buffer[..to_read])?;
                    remaining -= to_read as u64;
                }

                blocks_written += chunk.chunk_sz as u64;
            }
            CHUNK_TYPE_FILL => {
                // Fill pattern
                let mut fill_value = [0u8; 4];
                input.read_exact(&mut fill_value)?;

                let blocks_to_write = chunk.chunk_sz as u64;
                let bytes_to_write = blocks_to_write * header.blk_sz as u64;

                // Write fill pattern
                let fill_block = fill_value.repeat(header.blk_sz as usize / 4);
                for _ in 0..blocks_to_write {
                    output.write_all(&fill_block)?;
                }

                blocks_written += blocks_to_write;
            }
            CHUNK_TYPE_DONT_CARE => {
                // Skip/sparse - write zeros
                let blocks_to_write = chunk.chunk_sz as u64;
                let bytes_to_write = blocks_to_write * header.blk_sz as u64;

                let zero_block = vec![0u8; header.blk_sz as usize];
                for _ in 0..blocks_to_write {
                    output.write_all(&zero_block)?;
                }

                blocks_written += blocks_to_write;
            }
            _ => {
                return Err(anyhow!("Unknown chunk type: 0x{:X}", chunk.chunk_type));
            }
        }

        if (chunk_idx + 1) % 100 == 0 || chunk_idx + 1 == header.total_chunks {
            print!("\rConverting: {}/{} chunks", chunk_idx + 1, header.total_chunks);
            std::io::stdout().flush()?;
        }
    }

    println!("\nConversion complete: {} blocks written", blocks_written);
    output.flush()?;

    Ok(())
}
