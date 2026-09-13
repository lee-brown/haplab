//! QuickTime MOV atom identifiers and serialization helpers.

pub const ATOM_FTYP: [u8; 4] = *b"ftyp";
pub const ATOM_MOOV: [u8; 4] = *b"moov";
pub const ATOM_MVHD: [u8; 4] = *b"mvhd";
pub const ATOM_TRAK: [u8; 4] = *b"trak";
pub const ATOM_TKHD: [u8; 4] = *b"tkhd";
pub const ATOM_MDIA: [u8; 4] = *b"mdia";
pub const ATOM_MDHD: [u8; 4] = *b"mdhd";
pub const ATOM_HDLR: [u8; 4] = *b"hdlr";
pub const ATOM_MINF: [u8; 4] = *b"minf";
pub const ATOM_VMHD: [u8; 4] = *b"vmhd";
pub const ATOM_DINF: [u8; 4] = *b"dinf";
pub const ATOM_DREF: [u8; 4] = *b"dref";
pub const ATOM_STBL: [u8; 4] = *b"stbl";
pub const ATOM_STSD: [u8; 4] = *b"stsd";
pub const ATOM_STTS: [u8; 4] = *b"stts";
pub const ATOM_STSC: [u8; 4] = *b"stsc";
pub const ATOM_STSZ: [u8; 4] = *b"stsz";
pub const ATOM_STCO: [u8; 4] = *b"stco";
pub const ATOM_CO64: [u8; 4] = *b"co64";
pub const ATOM_MDAT: [u8; 4] = *b"mdat";

pub const BRAND_QT: [u8; 4] = *b"qt  ";
pub const HANDLER_VIDE: [u8; 4] = *b"vide";

/// Write a standard 8-byte atom header [size: u32 BE, type: [u8; 4]].
pub fn write_atom_header(atom_type: [u8; 4], total_size: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&total_size.to_be_bytes());
    out.extend_from_slice(&atom_type);
}

/// Wrap a payload into an atom [size: u32 BE, type: [u8; 4], payload].
pub fn create_atom(atom_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let total_size = (payload.len() + 8) as u32;
    let mut out = Vec::with_capacity(total_size as usize);
    out.extend_from_slice(&total_size.to_be_bytes());
    out.extend_from_slice(&atom_type);
    out.extend_from_slice(payload);
    out
}
