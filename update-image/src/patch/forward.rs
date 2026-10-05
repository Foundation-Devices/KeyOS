// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::BTreeMap,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    thread,
};

pub fn build(old: &[u8], new: &[u8]) -> io::Result<Vec<u8>> {
    let mut pieces = Vec::new();
    let mut source = 0i64;
    let mut target = 0;
    qbsdiff::Bsdiff::new(old, new)
        .small_match(super::SMALL_MATCH)
        .parallel_scheme(qbsdiff::ParallelScheme::Never)
        .with_controls(|controls| {
            for control in controls {
                let add = control.add as usize;
                if add > 0 {
                    assert!(source >= 0 && source as usize + add <= old.len(), "source outside file");
                    let mut offset = 0;
                    while offset < add {
                        let len = (add - offset).min(256);
                        pieces.push(Match { source: source as usize + offset, target: target + offset, len });
                        offset += len;
                    }
                }
                target += add + control.copy as usize;
                source += add as i64 + control.seek;
            }
        });
    assert_eq!(target, new.len(), "wrong control target size");

    // keep the longest chain whose source ranges move forward without overlap
    // example of a valid chain for reordered matches
    // old:   [a] [b] [c]
    // new:   [b] [a] [c]
    // patch: [read b] [literal a] [read c]
    // skipped matches become literals so source reads only move forward
    // source endpoints map to improving chain scores
    let mut chains = BTreeMap::from([(0, (0, usize::MAX))]);
    let mut previous = vec![usize::MAX; pieces.len()];
    let mut winner = (0, usize::MAX);
    for (i, r) in pieces.iter().enumerate() {
        let prior = *chains.range(..=r.source).next_back().unwrap().1;
        previous[i] = prior.1;
        let score = (prior.0 + r.len, i);
        if score.0 > winner.0 {
            winner = score;
        }
        let end = r.source + r.len;
        let (_, &(best, _)) = chains.range(..=end).next_back().unwrap();
        if best >= score.0 {
            continue;
        }
        // a better chain ending earlier makes these later endpoints redundant
        while let Some((&later, &(best, _))) = chains.range(end..).next() {
            if best > score.0 {
                break;
            }
            chains.remove(&later);
        }
        chains.insert(end, score);
    }
    let mut selected = Vec::new();
    let mut i = winner.1;
    while i != usize::MAX {
        selected.push(i);
        i = previous[i];
    }
    let mut ctrl = Vec::new();
    let mut delta = Vec::new();
    let mut extra = Vec::new();
    let mut source = 0;
    let mut target = 0;
    for i in selected.into_iter().rev() {
        let r = pieces[i];
        assert!(r.source >= source && r.target >= target, "nonmonotonic plan");
        let gap = r.target - target;
        for value in [0, gap as u64, (r.source - source) as u64, r.len as u64, 0, 0] {
            ctrl.extend_from_slice(&value.to_le_bytes());
        }
        extra.extend_from_slice(&new[target..r.target]);
        delta.extend(
            new[r.target..r.target + r.len]
                .iter()
                .zip(&old[r.source..r.source + r.len])
                .map(|(n, o)| n.wrapping_sub(*o)),
        );
        source = r.source + r.len;
        target = r.target + r.len;
    }
    for value in [0, (new.len() - target) as u64, 0] {
        ctrl.extend_from_slice(&value.to_le_bytes());
    }
    extra.extend_from_slice(&new[target..]);
    let (ctrl, delta, extra) = thread::scope(|scope| {
        let control = scope.spawn(|| compress(&ctrl, 20));
        let differences = scope.spawn(|| compress(&delta, 20));
        let literals = scope.spawn(|| compress(&extra, 22));
        Ok::<_, io::Error>((
            control.join().unwrap()?,
            differences.join().unwrap()?,
            literals.join().unwrap()?,
        ))
    })?;
    let mut body = b"BSDIFF4Z".to_vec();
    for value in [ctrl.len() as u64, delta.len() as u64, new.len() as u64] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    body.extend_from_slice(&ctrl);
    body.extend_from_slice(&delta);
    body.extend_from_slice(&extra);
    verify(&body, old, new)?;
    Ok(body)
}

fn verify(body: &[u8], old: &[u8], new: &[u8]) -> io::Result<u64> {
    struct Source<'a> {
        inner: Cursor<&'a [u8]>,
        end: u64,
        reads: u64,
    }
    impl Read for Source<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.end = self.inner.position();
            self.reads += 1;
            Ok(n)
        }
    }
    impl Seek for Source<'_> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let offset = self.inner.seek(pos)?;
            if offset < self.end {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "backward or overlapping source read",
                ));
            }
            Ok(offset)
        }
    }
    let mut source = Source { inner: Cursor::new(old), end: 0, reads: 0 };
    let mut target = Vec::with_capacity(new.len());
    super::apply(
        [Cursor::new(body), Cursor::new(body), Cursor::new(body)],
        body.len() as u64,
        &mut source,
        &mut [0; 65536],
        &mut target,
    )?;
    if target != new {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong decoded bytes"));
    }
    Ok(source.reads)
}

fn compress(bytes: &[u8], window: u32) -> io::Result<Vec<u8>> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 19)?;
    encoder.window_log(window)?;
    encoder.write_all(bytes)?;
    encoder.finish()
}

#[derive(Clone, Copy)]
struct Match {
    source: usize,
    target: usize,
    len: usize,
}

#[test]
fn reordered_source_round_trips_forward() {
    let mut state = 1u32;
    let old: Vec<u8> = (0..1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let mut new = old[512..].to_vec();
    new.extend_from_slice(b"inserted bytes");
    new.extend_from_slice(&old[..512]);
    new.extend_from_slice(&old[512..]);
    new[100] ^= 1;
    let first = build(&old, &new).unwrap();
    assert!(verify(&first, &old, &new).unwrap() > 0);
    assert_eq!(first, build(&old, &new).unwrap());
    for (old, new) in [(&[][..], &b"new file"[..]), (&b"deleted"[..], &[][..]), (&[][..], &[][..])] {
        verify(&build(old, new).unwrap(), old, new).unwrap();
    }
}
