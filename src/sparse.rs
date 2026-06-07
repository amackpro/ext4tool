use anyhow::{anyhow, Context, Result};
use byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};
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

/// Read the block size from an ext4 superblock (offset 0x18, log2(bs/1024)).
fn get_ext4_block_size<P: AsRef<Path>>(path: P) -> Result<u32> {
    let mut f = File::open(path.as_ref())?;
    // Superblock is at byte 1024 (EXT4_SUPERBLOCK_OFFSET)
    f.seek(SeekFrom::Start(1024 + 0x18))?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf)?;
    let log_bs = LittleEndian::read_u32(&buf);
    Ok(1024u32 << log_bs)
}

/// Write a raw ext4 image as an Android sparse image (magic 0xED26FF3A).
/// Detects all-zero blocks and emits DONT_CARE chunks for them.
pub fn write_sparse_image<P: AsRef<Path>>(raw_path: P, sparse_path: P) -> Result<()> {
    let blk_sz = get_ext4_block_size(&raw_path)?;

    let raw_meta = std::fs::metadata(raw_path.as_ref())
        .with_context(|| format!("Failed to stat {}", raw_path.as_ref().display()))?;
    let raw_len = raw_meta.len();
    let total_blks = (raw_len / blk_sz as u64) as u32;

    let mut raw = BufReader::new(File::open(raw_path.as_ref())?);
    let mut out = BufWriter::new(File::create(sparse_path.as_ref())?);

    // Buffer for one block
    let mut block = vec![0u8; blk_sz as usize];
    let zero_block = vec![0u8; blk_sz as usize];

    // Collect chunks: (chunk_type, num_blocks)
    // We'll build them in memory first to know total_chunks for the header.
    // For large images, this could use a lot of memory. For now, it's fine.
    // An alternative is a two-pass approach.
    struct Pending {
        chunk_type: u16,
        count: u32,
    }
    let mut chunks: Vec<Pending> = Vec::new();
    let mut current_type: u16 = 0;
    let mut current_count: u32 = 0;

    for _blk_idx in 0..total_blks {
        raw.read_exact(&mut block)?;
        let is_zero = block == zero_block;
        let expected_type = if is_zero {
            CHUNK_TYPE_DONT_CARE
        } else {
            CHUNK_TYPE_RAW
        };

        if current_count == 0 {
            current_type = expected_type;
            current_count = 1;
        } else if expected_type == current_type {
            current_count += 1;
        } else {
            chunks.push(Pending { chunk_type: current_type, count: current_count });
            current_type = expected_type;
            current_count = 1;
        }
    }
    if current_count > 0 {
        chunks.push(Pending { chunk_type: current_type, count: current_count });
    }

    // Write sparse header
    let header_sz: u16 = 28;
    let chunk_hdr_sz: u16 = 12;
    let total_chunks = chunks.len() as u32;

    out.write_u32::<LittleEndian>(SPARSE_HEADER_MAGIC)?;
    out.write_u16::<LittleEndian>(1)?;                      // major_version
    out.write_u16::<LittleEndian>(0)?;                      // minor_version
    out.write_u16::<LittleEndian>(header_sz)?;              // file_hdr_sz
    out.write_u16::<LittleEndian>(chunk_hdr_sz)?;           // chunk_hdr_sz
    out.write_u32::<LittleEndian>(blk_sz)?;                 // blk_sz
    out.write_u32::<LittleEndian>(total_blks)?;             // total_blks
    out.write_u32::<LittleEndian>(total_chunks)?;           // total_chunks
    out.write_u32::<LittleEndian>(0)?;                      // image_checksum

    // Rewind raw file for reading block data
    let mut raw_inner = File::open(raw_path.as_ref())?;

    // Write each chunk
    let mut block_offset: u64 = 0;
    for (i, chunk) in chunks.iter().enumerate() {
        let chunk_sz = chunk.count;
        match chunk.chunk_type {
            CHUNK_TYPE_RAW => {
                let data_sz = chunk_sz as u64 * blk_sz as u64;
                let total_sz = chunk_hdr_sz as u32 + data_sz as u32;

                out.write_u16::<LittleEndian>(CHUNK_TYPE_RAW)?;
                out.write_u16::<LittleEndian>(0)?;          // reserved
                out.write_u32::<LittleEndian>(chunk_sz)?;
                out.write_u32::<LittleEndian>(total_sz)?;

                // Copy raw data
                let mut remaining = data_sz;
                let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer
                raw_inner.seek(SeekFrom::Start(block_offset * blk_sz as u64))?;
                while remaining > 0 {
                    let to_read = remaining.min(buf.len() as u64) as usize;
                    raw_inner.read_exact(&mut buf[..to_read])?;
                    out.write_all(&buf[..to_read])?;
                    remaining -= to_read as u64;
                }
            }
            CHUNK_TYPE_DONT_CARE => {
                let total_sz = chunk_hdr_sz as u32;

                out.write_u16::<LittleEndian>(CHUNK_TYPE_DONT_CARE)?;
                out.write_u16::<LittleEndian>(0)?;          // reserved
                out.write_u32::<LittleEndian>(chunk_sz)?;
                out.write_u32::<LittleEndian>(total_sz)?;
                // No data to write
            }
            _ => unreachable!(),
        }
        block_offset += chunk_sz as u64;

        if (i + 1) % 500 == 0 || i + 1 == chunks.len() {
            print!("\rSparse conversion: {}/{} chunks", i + 1, chunks.len());
            std::io::stdout().flush()?;
        }
    }

    println!("\nSparse image written: {} blocks in {} chunks", total_blks, total_chunks);
    out.flush()?;

    Ok(())
}
