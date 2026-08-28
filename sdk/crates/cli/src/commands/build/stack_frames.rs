// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Report device functions that allocate an outsized stack frame.
//!
//! A KeyOS process gets 256 KiB of stack (`STACK_PAGE_COUNT` 64 pages), while a
//! host thread gets 8 MiB, so a frame that kills an app on device passes every
//! simulator run and every `cargo test`. The frames that do it are usually
//! compiler-generated rather than written: a derived `Clone` on a large enum
//! gives every match arm its own slot, so nothing in the source reads as
//! expensive and `size_of` says nothing useful.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

const OBJDUMP: &str = "arm-none-eabi-objdump";
/// An eighth of the process stack in a single frame is worth a second look.
const WARN_BYTES: u64 = 32 * 1024;
const PROCESS_STACK_KIB: u64 = 256;
const REPORTED: usize = 5;

struct Frame {
    function: String,
    bytes: u64,
}

/// Print the functions whose stack allocation is worth a look.
/// Never fails a build: it reports a measurement, and a missing objdump only
/// costs the measurement.
pub fn warn_on_large_frames(binary: &Path) {
    let frames = match objdump_frames(binary) {
        Ok(frames) => frames,
        Err(error) => {
            eprintln!("Skipped the stack-frame check: {error:#}");
            return;
        }
    };

    let mut over = frames.into_iter().filter(|frame| frame.bytes >= WARN_BYTES).collect::<Vec<_>>();
    if over.is_empty() {
        return;
    }
    over.sort_by(|left, right| right.bytes.cmp(&left.bytes));

    eprintln!();
    eprintln!(
        "Warning: {} function(s) allocate over {} KiB of stack in one frame, out of the {PROCESS_STACK_KIB} KiB a KeyOS process gets:",
        over.len(),
        WARN_BYTES / 1024
    );
    for frame in over.iter().take(REPORTED) {
        eprintln!("  {:>4} KiB  {}", frame.bytes / 1024, frame.function);
    }
    if over.len() > REPORTED {
        eprintln!("  and {} more", over.len() - REPORTED);
    }
    eprintln!(
        "The simulator will not reproduce a stack overflow here: a host thread has 8 MiB. A derived Clone on a large enum is the usual cause; take the value by value and mutate it in place so the clone is never generated."
    );
    eprintln!();
}

fn objdump_frames(binary: &Path) -> Result<Vec<Frame>> {
    let mut child = Command::new(OBJDUMP)
        .args(["--disassemble", "--demangle", "--section=.text"])
        .arg(binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("could not run {OBJDUMP}"))?;

    let stdout = child.stdout.take().context("could not read the disassembly")?;
    let frames = parse_frames(BufReader::new(stdout));
    let status = child.wait().with_context(|| format!("could not wait for {OBJDUMP}"))?;
    if !status.success() {
        bail!("{OBJDUMP} failed ({status})");
    }

    Ok(frames)
}

/// What each function subtracts from `sp` by immediate, in objdump order. A
/// frame too large for one Thumb-2 modified immediate is split over several
/// `sub sp`, so the immediates are summed. A frame built through a register is
/// not counted, which only loses functions this check would have reported.
fn parse_frames(disassembly: impl BufRead) -> Vec<Frame> {
    let mut frames: Vec<Frame> = Vec::new();
    let mut current: Option<String> = None;

    for line in disassembly.lines().map_while(Result::ok) {
        if let Some(name) = function_name(&line) {
            current = Some(name);
            continue;
        }
        let (Some(function), Some(bytes)) = (current.as_ref(), stack_allocation(&line)) else {
            continue;
        };
        match frames.last_mut() {
            Some(frame) if &frame.function == function => frame.bytes += bytes,
            _ => frames.push(Frame { function: function.clone(), bytes }),
        }
    }

    frames
}

/// `0037a09c <xous_api_names::XousNames>::request_connection>:` names a function.
fn function_name(line: &str) -> Option<String> {
    let (address, name) = line.strip_suffix(">:")?.split_once(" <")?;
    address.chars().all(|character| character.is_ascii_hexdigit()).then(|| name.to_string())
}

/// The immediate of a `sub sp, …, #N`, from objdump's tab-separated columns.
fn stack_allocation(line: &str) -> Option<u64> {
    let mut columns = line.split('\t').skip(2);
    if !matches!(columns.next()?.trim(), "sub" | "sub.w" | "subw") {
        return None;
    }
    // Requiring `sp` as the destination drops the conditional forms on other registers.
    let operands = columns.next()?.trim().strip_prefix("sp, ")?;
    operands.rsplit_once('#')?.1.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_frames;

    #[test]
    fn parse_frames_sums_the_immediates_of_a_split_prologue() {
        let disassembly = "\
0037a09c <<xous_api_names::XousNames>::request_connection>:
  37a09c:\tb5f0      \tpush\t{r4, r5, r6, r7, lr}
  37a0a4:\tf5ad 4d9f \tsub.w\tsp, sp, #20352\t@ 0x4f80
  37a1a4:\tf1a7 041c \tsubeq.w\tr4, r7, #28
  37a1a8:\tf5ad 4d80 \tsub.w\tsp, sp, #16384\t@ 0x4000
0037a1f8 <pgp::edit_key>:
  37a200:\tf5ad 3d31 \tsub.w\tsp, sp, #180224\t@ 0x2c000
  37a204:\t                \tsub\tsp, r3
0037a300 <small::function>:
  37a304:\te24dd0c8 \tsub\tsp, sp, #200
";
        let frames = parse_frames(disassembly.as_bytes());

        let reported = frames.iter().map(|frame| (frame.function.as_str(), frame.bytes)).collect::<Vec<_>>();
        assert_eq!(
            reported,
            vec![
                ("<xous_api_names::XousNames>::request_connection", 20352 + 16384),
                ("pgp::edit_key", 180224),
                ("small::function", 200),
            ]
        );
    }
}
