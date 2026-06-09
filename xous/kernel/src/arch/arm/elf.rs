// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::MemoryFlags,
    core::ops::Range,
    keyos::{is_address_encrypted, ASLR_END, ASLR_START, PAGE_SIZE, USER_AREA_END},
    xmas_elf::{
        header,
        program::{self, ProgramHeader32},
        ElfFile,
    },
    xous::Error,
};

/// Maximum number of PT_DYNAMIC entries to load.
const MAX_DYNAMIC_ENTRIES: usize = 256;

pub struct ElfLoadResult {
    /// The entry point address (with ASLR slide applied).
    pub entry_point: usize,

    /// The ASLR slide applied to the binary.
    /// Only used for backtraces after a crash.
    pub aslr_slide: usize,
}

/// Loads the elf file from the specified address and returns the entry point and ASLR slide.
/// Cleans up the temporary buffer.
pub fn load_elf(address: usize, len: usize) -> Result<ElfLoadResult, Error> {
    let elf_slice = unsafe { core::slice::from_raw_parts(address as _, len) };
    let elf_file = ElfFile::new(elf_slice).map_err(|_| Error::ParseError)?;
    if header::sanity_check(&elf_file).is_err() {
        return Err(Error::ParseError);
    }

    let machine = elf_file.header.pt2.machine().as_machine();
    let elf_type = elf_file.header.pt2.type_().as_type();
    let is_pie = matches!(elf_type, header::Type::SharedObject); // ET_DYN
    let is_legacy = matches!(elf_type, header::Type::Executable); // ET_EXEC
    if machine != header::Machine::Arm || !(is_legacy || is_pie) {
        return Err(Error::ParseError);
    }

    let mut mapping = crate::arch::mem::MemoryMapping::current();
    let mut headers = [ProgramHeader32::default(); 32];
    let mut header_count = 0;

    // Track PT_DYNAMIC and PT_GNU_RELRO (reject multiples)
    let mut dynamic_vaddr: Option<u32> = None;
    let mut dynamic_filesz: u32 = 0;

    let mut relro_vaddr: Option<u32> = None;
    let mut relro_size: u32 = 0;

    // Image span (original) for ASLR choice & validation
    let mut min_vaddr = u32::MAX;
    let mut max_vaddr = 0u32;

    // Overlap checking (pagewise)
    let mut load_ranges: [Range<usize>; 32] = core::array::from_fn(|_| 0..0);
    let mut load_range_count = 0usize;

    let mut writable_load_ranges: [Range<usize>; 32] = core::array::from_fn(|_| 0..0);
    let mut writable_count = 0usize;

    let mut exec_load_ranges: [Range<usize>; 32] = core::array::from_fn(|_| 0..0);
    let mut exec_count = 0usize;

    for ph in elf_file.program_iter() {
        if program::sanity_check(ph, &elf_file).is_err() {
            return Err(Error::ParseError);
        }

        let program::ProgramHeader::Ph32(h) = ph else { continue };

        match h.get_type() {
            Ok(program::Type::Load) => {
                if header_count >= headers.len() {
                    return Err(Error::ParseError);
                }

                // ELF invariant: p_memsz >= p_filesz
                if h.mem_size < h.file_size {
                    return Err(Error::ParseError);
                }

                let segment_virtual_addr = h.virtual_addr as usize;
                let mem_size = h.mem_size as usize;
                let end_unaligned = segment_virtual_addr.checked_add(mem_size).ok_or(Error::ParseError)?;
                let seg_range = segment_virtual_addr..end_unaligned;
                let start_page = align_down(segment_virtual_addr, PAGE_SIZE);
                let end_page = align_up(end_unaligned, PAGE_SIZE)?;

                if load_range_count >= load_ranges.len() {
                    return Err(Error::ParseError);
                }
                load_ranges[load_range_count] = start_page..end_page;
                load_range_count += 1;

                headers[header_count] = *h;
                header_count += 1;

                min_vaddr = min_vaddr.min(h.virtual_addr);
                max_vaddr = max_vaddr.max(h.virtual_addr.checked_add(h.mem_size).ok_or(Error::ParseError)?);

                // Disallow explicit W+X
                if h.flags.is_write() && h.flags.is_execute() {
                    return Err(Error::ParseError);
                }

                if h.flags.is_write() {
                    if writable_count >= writable_load_ranges.len() {
                        return Err(Error::ParseError);
                    }
                    writable_load_ranges[writable_count] = seg_range.clone();
                    writable_count += 1;
                }

                if h.flags.is_execute() {
                    if exec_count >= exec_load_ranges.len() {
                        return Err(Error::ParseError);
                    }
                    exec_load_ranges[exec_count] = seg_range;
                    exec_count += 1;
                }
            }
            Ok(program::Type::Dynamic) => {
                if dynamic_vaddr.is_some() {
                    // Already saw one, reject the ELF with multiple Dynamic entries
                    return Err(Error::ParseError);
                }
                dynamic_vaddr = Some(h.virtual_addr);
                dynamic_filesz = h.file_size;
            }
            Ok(program::Type::GnuRelro) => {
                if relro_vaddr.is_some() {
                    // Already saw one, reject the ELF with multiple RELRO entries
                    return Err(Error::ParseError);
                }
                relro_vaddr = Some(h.virtual_addr);
                relro_size = h.mem_size;
            }
            _ => {}
        }
    }

    if header_count == 0 {
        return Err(Error::ParseError);
    }

    // Reject overlapping PT_LOAD page ranges
    for i in 0..load_range_count {
        for j in (i + 1)..load_range_count {
            if ranges_overlap(&load_ranges[i], &load_ranges[j]) {
                return Err(Error::ParseError);
            }
        }
    }

    // Compute original image span in pages
    let image_min_page = align_down(min_vaddr as usize, PAGE_SIZE);
    let image_max_page = align_up(max_vaddr as usize, PAGE_SIZE)?;
    let image_span = image_max_page.checked_sub(image_min_page).ok_or(Error::ParseError)?;
    let image_original: Range<usize> = image_min_page..image_max_page;

    // Choose ASLR slide for PIE
    let aslr_slide = if is_pie {
        if dynamic_vaddr.is_none() {
            return Err(Error::ParseError);
        }
        choose_aslr_slide(image_min_page, image_span)?
    } else {
        0
    };

    // Entrypoint must be within the loaded image span (original)
    let entry_original = elf_file.header.pt2.entry_point() as usize;
    if !image_original.contains(&entry_original) {
        return Err(Error::ParseError);
    }
    let entry_point = checked_add_aslr_slide(entry_original, aslr_slide)?;

    crate::mem::MemoryManager::with_mut(|mm| {
        for header in headers.iter().take(header_count) {
            let mut header = *header; // local mutable copy; stored headers remain intact

            // Bounds check for file-backed segment data (slice-safe)
            if header.file_size > 0 {
                let end = header.offset.checked_add(header.file_size).ok_or(Error::ParseError)?;
                if end > len as u32 {
                    return Err(Error::ParseError);
                }
            }

            let seg_start = checked_add_aslr_slide(header.virtual_addr as usize, aslr_slide)?;
            let seg_end_original =
                header.virtual_addr.checked_add(header.mem_size).ok_or(Error::ParseError)? as usize;
            let seg_end = checked_add_aslr_slide(seg_end_original, aslr_slide)?;

            if is_pie {
                let win_start = align_up(ASLR_START, PAGE_SIZE)?;
                let win_end = align_down(ASLR_END, PAGE_SIZE);
                if seg_start < win_start || seg_end > win_end {
                    return Err(Error::ParseError);
                }
            } else if seg_end > USER_AREA_END {
                return Err(Error::ParseError);
            }

            let mut final_flags = MemoryFlags::empty();
            if header.flags.is_write() {
                final_flags |= MemoryFlags::W;
            }
            if header.flags.is_execute() {
                final_flags |= MemoryFlags::X;
            }

            // Abort if final_flags somehow violate W^X
            if final_flags.is_set(MemoryFlags::W | MemoryFlags::X) {
                return Err(Error::ParseError);
            }

            let copy_flags = MemoryFlags::W | MemoryFlags::POPULATE;

            let src_start = (address + header.offset as usize) as *mut usize;
            let has_to_be_encrypted = header.flags.is_write()
                && !is_address_encrypted(mapping.virt_to_phys(src_start).unwrap_or(0));

            while header.mem_size > 0 {
                // Fast path: remap an entire page if aligned and safe
                if is_aligned(header.virtual_addr)
                    && is_aligned(header.offset)
                    && header.mem_size as usize >= PAGE_SIZE
                    && header.file_size as usize >= PAGE_SIZE
                    && !has_to_be_encrypted
                {
                    let dst = checked_add_aslr_slide(header.virtual_addr as usize, aslr_slide)?;

                    let src = (address + header.offset as usize) as *mut usize;
                    let phys = mapping.virt_to_phys(src)?;
                    mapping.unmap_page(src)?;
                    mapping.map_page(mm, phys, dst as _, final_flags, true)?;

                    header.virtual_addr += PAGE_SIZE as u32;
                    header.offset += PAGE_SIZE as u32;
                    header.mem_size -= PAGE_SIZE as u32;
                    header.file_size -= PAGE_SIZE as u32;
                } else {
                    // Slow path: handle partial pages or special cases
                    let vaddr_page_orig = align_down(header.virtual_addr as usize, PAGE_SIZE);
                    let vaddr_page = checked_add_aslr_slide(vaddr_page_orig, aslr_slide)?;

                    let mapped_size = (vaddr_page_orig
                        .checked_add(PAGE_SIZE)
                        .ok_or(Error::ParseError)?
                        .checked_sub(header.virtual_addr as usize)
                        .ok_or(Error::ParseError)?)
                    .min(header.mem_size as usize);

                    if header.file_size > 0 {
                        let copy_size = (header.file_size as usize).min(mapped_size);
                        let src_off = header.offset as usize;
                        let src_end = src_off.checked_add(copy_size).ok_or(Error::ParseError)?;
                        if src_end > len {
                            return Err(Error::ParseError);
                        }
                        let src = &elf_slice[src_off..src_end];

                        let dst_ptr =
                            checked_add_aslr_slide(header.virtual_addr as usize, aslr_slide)? as *mut u8;

                        // Map W, copy, then remap to final flags (W^X preserved)
                        mm.map_range(0, vaddr_page as _, PAGE_SIZE, copy_flags, true)?;
                        unsafe {
                            let dest = core::slice::from_raw_parts_mut(dst_ptr, copy_size);
                            dest.copy_from_slice(src);
                        }
                        remap_page_flags(mm, &mut mapping, vaddr_page, final_flags)?;

                        header.offset =
                            header.offset.checked_add(copy_size as u32).ok_or(Error::ParseError)?;
                        header.file_size =
                            header.file_size.checked_sub(copy_size as u32).ok_or(Error::ParseError)?;
                    } else {
                        mm.map_range(0, vaddr_page as _, PAGE_SIZE, final_flags, true)?;
                    }

                    header.virtual_addr =
                        header.virtual_addr.checked_add(mapped_size as u32).ok_or(Error::ParseError)?;
                    header.mem_size =
                        header.mem_size.checked_sub(mapped_size as u32).ok_or(Error::ParseError)?;
                }
            }
        }

        // PIE relocations + RELRO
        if is_pie {
            let dyn_vaddr = dynamic_vaddr.ok_or(Error::ParseError)?;
            let dyn_start = dyn_vaddr as usize;
            let dyn_end = dyn_start.checked_add(dynamic_filesz as usize).ok_or(Error::ParseError)?;

            // PT_DYNAMIC must lie within the loaded image span (original)
            let dyn_range = dyn_start..dyn_end;
            if !range_contains_range(&image_original, &dyn_range) {
                return Err(Error::ParseError);
            }

            let (rel_ptr, rel_sz, _rel_ent) = parse_dynamic_rel_info(aslr_slide, dyn_vaddr, dynamic_filesz)?;

            // Relocation table must lie within the image span (original)
            let rel_start = rel_ptr as usize;
            let rel_end = rel_start.checked_add(rel_sz as usize).ok_or(Error::ParseError)?;
            let rel_range = rel_start..rel_end;
            if !range_contains_range(&image_original, &rel_range) {
                return Err(Error::ParseError);
            }

            apply_relocations(
                aslr_slide,
                rel_ptr,
                rel_sz,
                &image_original,
                &writable_load_ranges[..writable_count],
            )?;

            // Apply RELRO (must lie within image span)
            if let (Some(rva), rsz) = (relro_vaddr, relro_size) {
                let relro_start = rva as usize;
                let relro_end = relro_start.checked_add(rsz as usize).ok_or(Error::ParseError)?;
                if !range_contains_range(&image_original, &(relro_start..relro_end)) {
                    return Err(Error::ParseError);
                }

                let start = checked_add_aslr_slide(rva as usize, aslr_slide)?;
                mark_pages_read_only(mm, &mut mapping, start, rsz as usize)?;
            }
        }

        // Unmap the rest of the pages that held the raw ELF image
        // This will try to unmap pages that were already moved, which results in an error, but that's OK
        mm.unmap_range(address as _, len).ok();

        Ok(ElfLoadResult { entry_point, aslr_slide })
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf32Dyn {
    d_tag: i32,
    d_val: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf32Rel {
    r_offset: u32,
    r_info: u32,
}

const DT_NULL: i32 = 0;
const DT_REL: i32 = 17;
const DT_RELSZ: i32 = 18;
const DT_RELENT: i32 = 19;

const R_ARM_RELATIVE: u32 = 23;

const MAX_RELOCS: usize = 200_000; // cap relocation count (DoS hardening)
const MAX_REL_BYTES: usize = MAX_RELOCS * core::mem::size_of::<Elf32Rel>();

#[inline(always)]
fn elf32_r_type(info: u32) -> u32 { info & 0xff }

#[inline]
fn align_down(x: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    x & !(a - 1)
}

#[inline]
fn align_up(x: usize, a: usize) -> Result<usize, Error> {
    debug_assert!(a.is_power_of_two());
    x.checked_next_multiple_of(a).ok_or(Error::ParseError)
}

const _: () = assert!(ASLR_START.is_multiple_of(PAGE_SIZE), "ASLR_START is not a multiple of PAGE_SIZE");
const _: () = assert!(ASLR_END.is_multiple_of(PAGE_SIZE), "ASLR_END is not a multiple of PAGE_SIZE");
const _: () = assert!(ASLR_END >= ASLR_START);
const _: () = assert!(ASLR_END - ASLR_START >= PAGE_SIZE);

fn ranges_overlap(a: &Range<usize>, b: &Range<usize>) -> bool { a.start < b.end && b.start < a.end }

fn range_contains_range(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    // Reject empty or inverted inner ranges so that, for example, an empty range at
    // the end of `outer` (outer.end..outer.end) is not treated as contained.
    if inner.start >= inner.end {
        return false;
    }
    inner.start >= outer.start && inner.end <= outer.end
}

fn is_aligned(v: u32) -> bool { v as usize & (PAGE_SIZE - 1) == 0 }

fn checked_add_aslr_slide(v: usize, slide: usize) -> Result<usize, Error> {
    v.checked_add(slide).ok_or(Error::ParseError)
}

/// Choose a random base for a PIE image so that the whole image fits inside [ASLR_START, ASLR_END).
fn choose_aslr_slide(min_vaddr_page: usize, span: usize) -> Result<usize, Error> {
    if ASLR_START >= ASLR_END {
        return Err(Error::ParseError);
    }
    if span == 0 || span > (ASLR_END - ASLR_START) {
        return Err(Error::ParseError);
    }

    let max_base = ASLR_END - span;
    let pages_available = (max_base - ASLR_START) / PAGE_SIZE;

    let page = if pages_available == 0 {
        0
    } else {
        let r = crate::platform::rand::get_u32() as usize;
        r % (pages_available + 1)
    };

    let base = ASLR_START + page * PAGE_SIZE;

    base.checked_sub(min_vaddr_page).ok_or(Error::ParseError)
}

/// Remap an existing mapped page at `va` to `new_flags`.
fn remap_page_flags(
    mm: &mut crate::mem::MemoryManager,
    mapping: &mut crate::arch::mem::MemoryMapping,
    va: usize,
    new_flags: MemoryFlags,
) -> Result<(), Error> {
    let phys = mapping.virt_to_phys(va as _)?;
    mapping.unmap_page(va as _)?;
    mapping.map_page(mm, phys, va as _, new_flags, true)?;
    Ok(())
}

/// Mark a range [start, start+size) as read-only, page-by-page.
fn mark_pages_read_only(
    mm: &mut crate::mem::MemoryManager,
    mapping: &mut crate::arch::mem::MemoryMapping,
    start: usize,
    size: usize,
) -> Result<(), Error> {
    if size == 0 {
        return Ok(());
    }
    let mut va = align_down(start, PAGE_SIZE);
    let end = align_up(start.checked_add(size).ok_or(Error::ParseError)?, PAGE_SIZE)?;

    // Empty flags mean read-only, non-executable
    let flags = MemoryFlags::empty();

    while va < end {
        remap_page_flags(mm, mapping, va, flags)?;
        va = va.checked_add(PAGE_SIZE).ok_or(Error::ParseError)?;
    }
    Ok(())
}

fn parse_dynamic_rel_info(
    slide: usize,
    dynamic_vaddr_orig: u32,
    dynamic_filesz: u32,
) -> Result<(u32, u32, u32), Error> {
    let dyn_ptr = checked_add_aslr_slide(dynamic_vaddr_orig as usize, slide)? as *const Elf32Dyn;

    let mut rel_ptr: u32 = 0;
    let mut rel_sz: u32 = 0;
    let mut rel_ent: u32 = 0;

    let num_entries = dynamic_filesz as usize / core::mem::size_of::<Elf32Dyn>();
    if num_entries == 0 || num_entries > MAX_DYNAMIC_ENTRIES {
        return Err(Error::ParseError);
    }

    let mut saw_dt_null = false;
    for i in 0..num_entries {
        let d = unsafe { core::ptr::read_unaligned(dyn_ptr.add(i)) };
        if d.d_tag == DT_NULL {
            saw_dt_null = true;
            break;
        }
        match d.d_tag {
            DT_REL => rel_ptr = d.d_val,
            DT_RELSZ => rel_sz = d.d_val,
            DT_RELENT => rel_ent = d.d_val,
            _ => {}
        }
    }

    // Reject ELF if no DT_NULL was encountered
    if !saw_dt_null {
        return Err(Error::ParseError);
    }

    if rel_ptr == 0 || rel_sz == 0 || rel_ent as usize != core::mem::size_of::<Elf32Rel>() {
        return Err(Error::ParseError);
    }
    if (rel_sz as usize) > MAX_REL_BYTES {
        return Err(Error::ParseError);
    }
    if !(rel_sz as usize).is_multiple_of(core::mem::size_of::<Elf32Rel>()) {
        return Err(Error::ParseError);
    }

    Ok((rel_ptr, rel_sz, rel_ent))
}

fn apply_relocations(
    slide: usize,
    rel_ptr: u32,
    rel_sz: u32,
    image_original: &Range<usize>,
    writable_load_ranges: &[Range<usize>],
) -> Result<(), Error> {
    let count = rel_sz as usize / core::mem::size_of::<Elf32Rel>();
    if count > MAX_RELOCS {
        return Err(Error::ParseError);
    }

    let rel_base = checked_add_aslr_slide(rel_ptr as usize, slide)? as *const Elf32Rel;

    for i in 0..count {
        let rel = unsafe { core::ptr::read_unaligned(rel_base.add(i)) };
        if elf32_r_type(rel.r_info) != R_ARM_RELATIVE {
            return Err(Error::ParseError);
        }

        let relocation_offset = rel.r_offset as usize;

        // Must be inside the loaded image span (original addresses).
        // Note: we allow unaligned offsets because packed C structs (e.g. the SDK's
        // nbgl_icon_details_t) place pointers at non-4-byte-aligned offsets.
        // The read/write below uses read_unaligned/write_unaligned so this is safe.
        let dst_end = relocation_offset.checked_add(4).ok_or(Error::ParseError)?;
        if relocation_offset < image_original.start || dst_end > image_original.end {
            return Err(Error::ParseError);
        }

        // Relocation destinations must land in a writable PT_LOAD.
        let in_writable =
            writable_load_ranges.iter().any(|r| r.start <= relocation_offset && dst_end <= r.end);
        if !in_writable {
            return Err(Error::ParseError);
        }

        let where_ptr = checked_add_aslr_slide(relocation_offset, slide)? as *mut u32;
        let addend = unsafe { core::ptr::read_unaligned(where_ptr) };
        let slide_u32: u32 = slide.try_into().map_err(|_| Error::ParseError)?;
        let relocated = addend.checked_add(slide_u32).ok_or(Error::ParseError)?;
        unsafe { core::ptr::write_unaligned(where_ptr, relocated) };
    }

    Ok(())
}
