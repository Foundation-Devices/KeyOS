mod api;

use api::*;

mod platform;
use num_traits::*;
use platform::Trng;
use trng::TrngSource;

fn main() -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::Highest).unwrap();

    let trng_sid = xous::create_server_with_sid(xous::SID::from_bytes(SERVER_NAME_TRNG).unwrap(), 0..8)
        .expect("Couldn't create server");
    log::trace!("Created server -- {:x?}", trng_sid);

    let mut trng = Trng::new();

    loop {
        let mut msg = xous::receive_message(trng_sid).unwrap();
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(api::Opcode::GetTrng) => {
                if matches!(msg.body, xous::Message::BlockingScalar(_)) {
                    let mut val = [0u32; 2];
                    trng.fill_buf(&mut val, TrngSource::Combined);
                    xous::return_scalar2(msg.sender, val[0] as _, val[1] as _).ok();
                } else {
                    log::error!("GetTrng Message was not a scalar")
                }
            }
            Some(api::Opcode::FillTrng) => {
                if let xous::Message::MutableBorrow(ref mut mem_msg) = msg.body {
                    let mut slice = mem_msg.buf.as_slice_mut();
                    if let Some(valid) = mem_msg.valid {
                        if valid.get() < slice.len() {
                            slice = &mut slice[0..valid.get()]
                        }
                    }
                    let rng_source = TrngSource::from_usize(mem_msg.offset.map(|n| n.get()).unwrap_or(0))
                        .unwrap_or(TrngSource::Combined);
                    trng.fill_buf(slice, rng_source);
                } else {
                    if matches!(msg.body, xous::Message::BlockingScalar(_)) {
                        xous::return_scalar(msg.sender, 0).ok();
                    }
                    log::error!("FillTrng Message was not LendMut")
                }
            }
            Some(api::Opcode::FillAvalancheRaw) => {
                if let xous::Message::MutableBorrow(ref mut mem_msg) = msg.body {
                    let mut slice = mem_msg.buf.as_slice_mut();
                    if let Some(valid) = mem_msg.valid {
                        if valid.get() < slice.len() {
                            slice = &mut slice[0..valid.get()]
                        }
                    }
                    trng.fill_avalanche_raw_samples(slice);
                } else {
                    if matches!(msg.body, xous::Message::BlockingScalar(_)) {
                        xous::return_scalar(msg.sender, 0).ok();
                    }
                    log::error!("FillAvalancheRaw Message was not LendMut")
                }
            }
            None => {
                log::error!("couldn't convert opcode, ignoring");
            }
        }
    }
}
