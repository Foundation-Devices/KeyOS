// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! UART + DMA RX demo

#![no_std]
#![no_main]

use {
    atsama5d27::{
        aes::{Aes, AesMode, Iv, Key},
        aic::{Aic, InterruptEntry, SourceKind},
        dma::{DmaChannel, Xdmac, XdmacChannel},
        pit::{Pit, PIV_MAX},
        pmc::{PeripheralId, Pmc},
        uart::{Uart, Uart1},
    },
    core::{
        arch::global_asm,
        fmt::Write,
        panic::PanicInfo,
        ptr::addr_of_mut,
        sync::atomic::{
            compiler_fence,
            AtomicBool,
            Ordering::{self, SeqCst},
        },
    },
    hex_literal::hex,
};

global_asm!(include_str!("../start.S"));

type UartType = Uart<Uart1>;

// MCK: 164MHz
// Clock frequency is divided by 2 because of the default `h32mxdiv` PMC setting
const MASTER_CLOCK_SPEED: u32 = 164000000 / 2;

const AES_BLOCK_SIZE: usize = 16;
const DMA_BUF_SIZE_BLOCKS: usize = 64;

#[repr(align(4))]
struct Align<T>(pub T);

static mut INPUT: Align<[u8; AES_BLOCK_SIZE * DMA_BUF_SIZE_BLOCKS]> =
    Align([0; AES_BLOCK_SIZE * DMA_BUF_SIZE_BLOCKS]);
static mut OUTPUT: Align<[u8; AES_BLOCK_SIZE * DMA_BUF_SIZE_BLOCKS]> =
    Align([0; AES_BLOCK_SIZE * DMA_BUF_SIZE_BLOCKS]);

static DMA_CH0_COMPLETE: AtomicBool = AtomicBool::new(false);
static DMA_CH1_COMPLETE: AtomicBool = AtomicBool::new(false);

const AES_TX_DMA_CHANNEL: DmaChannel = DmaChannel::Channel0;
const AES_RX_DMA_CHANNEL: DmaChannel = DmaChannel::Channel1;

#[no_mangle]
fn _entry() -> ! {
    extern "C" {
        // These symbols come from `link.ld`
        static mut _sbss: u32;
        static mut _ebss: u32;
    }

    // Initialize RAM
    unsafe {
        r0::zero_bss(addr_of_mut!(_sbss), addr_of_mut!(_ebss));
    }

    atsama5d27::l1cache::disable_dcache();

    let mut pmc = Pmc::new();
    pmc.enable_peripheral_clock(PeripheralId::Pit);
    pmc.enable_peripheral_clock(PeripheralId::Aic);
    pmc.enable_peripheral_clock(PeripheralId::Pioa);
    pmc.enable_peripheral_clock(PeripheralId::Piob);
    pmc.enable_peripheral_clock(PeripheralId::Pioc);
    pmc.enable_peripheral_clock(PeripheralId::Piod);
    pmc.enable_peripheral_clock(PeripheralId::Xdmac1);
    pmc.enable_peripheral_clock(PeripheralId::Aes);

    let mut aic = Aic::new();
    aic.init();

    let xdmac_irq_ptr = xdmac_irq_handler as unsafe extern "C" fn() as usize;
    aic.set_interrupt_handler(InterruptEntry {
        peripheral_id: PeripheralId::Xdmac1,
        vector_fn_ptr: xdmac_irq_ptr,
        kind: SourceKind::LevelSensitive,
        priority: 0,
    });

    // Timer for delays
    let mut pit = Pit::new();
    pit.set_interval(PIV_MAX);
    pit.set_enabled(true);
    pit.set_clock_speed(MASTER_CLOCK_SPEED);
    pit.busy_wait_ms(MASTER_CLOCK_SPEED, 500);

    let mut uart = UartType::new();
    uart.set_rx(true);

    writeln!(
        uart,
        "input addr: {:08x}, output addr: {:08x}",
        unsafe { INPUT.0.as_ptr() as usize },
        unsafe { OUTPUT.0.as_ptr() as usize },
    )
    .ok();

    run_tests();

    loop {
        armv7::asm::wfi();
    }
}

const TEST_VECTORS: &[TestVector] = &[
    TestVector::ecb_encrypt(
        &hex!("8000000000000000000000000000000000000000000000000000000000000000"),
        &hex!("00000000000000000000000000000000"),
        &hex!("E35A6DCB19B201A01EBCFA8AA22B5759")
    ),
    TestVector::ecb_encrypt(
        &hex!("0000000000000000000000000000000000000000000000000000000000000000"),
        &hex!("00000000000000000000000000000000"),
        &hex!("DC95C078A2408989AD48A21492842087")
    ),
    TestVector::ecb_decrypt(
        &hex!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        &hex!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        &hex!("F5DDE67E6999DC9A24E3B510F651EA6B"),
    ),
    TestVector::ecb_decrypt(
        &hex!("7878787878787878787878787878787878787878787878787878787878787878"),
        &hex!("2B0E74C9B4F4865F0C5F0D48701166E6"),
        &hex!("78787878787878787878787878787878"),
    ),
    TestVector::cbc_encrypt(
        &hex!("e09eaa5a3f5e56d279d5e7a03373f6ea"),
        &hex!("c9ee3cd746bf208c65ca9e72a266d54f"),
        &hex!("ef4eab37181f98423e53e947e7050fd0"),
        &hex!("d1fa697f3e2e04d64f1a0da203813ca5"),
    ),
    TestVector::cbc_encrypt(
        &hex!("9bd3902ed0996c869b572272e76f3889"),
        &hex!("8b2e86a9a185cfa6f51c7cc595b822bc"),
        &hex!("a7ba19d49ee1ea02f098aa8e30c740d893a4456ccc294040484ed8a00a55f93e"),
        &hex!("514cbc69aced506926deacdeb0cc0a5a07d540f65d825b65c7db0075cf930a06"),
    ),
    TestVector::cbc_decrypt(
        &hex!("9bd3902ed0996c869b572272e76f3889"),
        &hex!("8b2e86a9a185cfa6f51c7cc595b822bc"),
        &hex!("514cbc69aced506926deacdeb0cc0a5a07d540f65d825b65c7db0075cf930a06"),
        &hex!("a7ba19d49ee1ea02f098aa8e30c740d893a4456ccc294040484ed8a00a55f93e"),
    ),
    TestVector::cbc_encrypt(
        &hex!("2b7e151628aed2a6abf7158809cf4f3c"),
        &hex!("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"),
        &hex!("006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000"),
        &hex!("6544CCA076C4D67C1A69DD7E504C6586FBD22912505E187D8628E19FA067D6C339D078E3032B8596DA74BB0E23434F83E153D5ACD5DEF7D264F58EC685317BF50C93430791718D6E09CCC4804FFE4EEB5C6AD8E9B5DFD456EDE81081627A97FC2FAE9F1955377D7774E68EAB541B20CE3C915185BCA208EE08428C400043F2DC90B0390756762C9271946FCE214B9576F74399E466DAC48C6DD10B420F302941DCC27D55CF1FB59D71954950CAD893FFFA70970D128C77BFA34F3C84B0B64A01194A086ACDD9847D6B91B7F870D0E7591CA07F0B407005F1473C37A648F6E18044336F30418BA43FD7AA5B5BAE01A0E33B1EDA4487730F043E202DE44CB901BD3AED13D790D05F325C414831EB601BD918678C1B8E116877CE1167F87204B49619D323713F95C04CA9621FDCF44BD21C5E36A299C486C8FC0D3043EDFF424B9A7AA5500DC3BD7BF6FAB256E6B45B458058DC933F1FF8C5E841BFC7F405761E14B12B48C1C108F33BF8D65BB8DBB9ED7E92398E779333730F4C68922AA76409E842E76B649B981B8269186220ACFF9DFA198D62CBF4CFA0FE05C1427CE63A345A61FE460D14EF25D7A89E2E228B415757B4E4110B6AFA7D85D48C3BCF184FDD7366F06D9E3D29896B0D3C0D83FCFA881E6EC5F29B0294628EDFF284E58B7BE19D37A6B28D70DC0F165A4B60CE5536D76D1A71849C36B0837E4E5082A05208CEEB320C57F0F5B86DC3CAAC8A32DEA9552D"),
    ),
    TestVector::cbc_decrypt(
        &hex!("2b7e151628aed2a6abf7158809cf4f3c"),
        &hex!("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"),
        &hex!("6544CCA076C4D67C1A69DD7E504C6586FBD22912505E187D8628E19FA067D6C339D078E3032B8596DA74BB0E23434F83E153D5ACD5DEF7D264F58EC685317BF50C93430791718D6E09CCC4804FFE4EEB5C6AD8E9B5DFD456EDE81081627A97FC2FAE9F1955377D7774E68EAB541B20CE3C915185BCA208EE08428C400043F2DC90B0390756762C9271946FCE214B9576F74399E466DAC48C6DD10B420F302941DCC27D55CF1FB59D71954950CAD893FFFA70970D128C77BFA34F3C84B0B64A01194A086ACDD9847D6B91B7F870D0E7591CA07F0B407005F1473C37A648F6E18044336F30418BA43FD7AA5B5BAE01A0E33B1EDA4487730F043E202DE44CB901BD3AED13D790D05F325C414831EB601BD918678C1B8E116877CE1167F87204B49619D323713F95C04CA9621FDCF44BD21C5E36A299C486C8FC0D3043EDFF424B9A7AA5500DC3BD7BF6FAB256E6B45B458058DC933F1FF8C5E841BFC7F405761E14B12B48C1C108F33BF8D65BB8DBB9ED7E92398E779333730F4C68922AA76409E842E76B649B981B8269186220ACFF9DFA198D62CBF4CFA0FE05C1427CE63A345A61FE460D14EF25D7A89E2E228B415757B4E4110B6AFA7D85D48C3BCF184FDD7366F06D9E3D29896B0D3C0D83FCFA881E6EC5F29B0294628EDFF284E58B7BE19D37A6B28D70DC0F165A4B60CE5536D76D1A71849C36B0837E4E5082A05208CEEB320C57F0F5B86DC3CAAC8A32DEA9552D"),
        &hex!("006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000006bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c371000"),
    ),
    TestVector::xts_encrypt(
        &hex!("00000000000000000000000000000000"),
        &hex!("00000000000000000000000000000000"),
        &hex!("00000000000000000000000000000000"),
        &hex!("0000000000000000000000000000000000000000000000000000000000000000"),
        &hex!("917cf69ebd68b2ec9b9fe9a3eadda692cd43d2f59598ed858c02c2652fbf922e")
    ),
    TestVector::xts_decrypt(
        &hex!("0000000000000000000000000000000000000000000000000000000000000000"),
        &hex!("0000000000000000000000000000000000000000000000000000000000000000"),
        &hex!("00000000000000000000000000000000"),
        &hex!("d456b4fc2e620bba6ffbed27b956c9543454dd49ebd8d8ee6f94b65cbe158f73"),
        &hex!("0000000000000000000000000000000000000000000000000000000000000000")
    ),
    TestVector::xts_decrypt(
        &hex!("fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0"),
        &hex!("22222222222222222222222222222222"),
        &hex!("33333333330000000000000000000000"),
        &hex!("af85336b597afc1a900b2eb21ec949d292df4c047e0b21532186a5971a227a89"),
        &hex!("4444444444444444444444444444444444444444444444444444444444444444")
    ),
    TestVector::xts_encrypt(
        &hex!("27182818284590452353602874713526"),
        &hex!("31415926535897932384626433832795"),
        &hex!("00000000000000000000000000000000"),
        &hex!(
            r#"
             000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
             202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f
             404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f
             606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f
             808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f
             a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf
             c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf
             e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff
             000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
             202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f
             404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f
             606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f
             808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f
             a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf
             c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf
             e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff
            "#
        ),
        &hex!(
            r#"
            27a7479befa1d476489f308cd4cfa6e2a96e4bbe3208ff25287dd3819616e89c
            c78cf7f5e543445f8333d8fa7f56000005279fa5d8b5e4ad40e736ddb4d35412
            328063fd2aab53e5ea1e0a9f332500a5df9487d07a5c92cc512c8866c7e860ce
            93fdf166a24912b422976146ae20ce846bb7dc9ba94a767aaef20c0d61ad0265
            5ea92dc4c4e41a8952c651d33174be51a10c421110e6d81588ede82103a252d8
            a750e8768defffed9122810aaeb99f9172af82b604dc4b8e51bcb08235a6f434
            1332e4ca60482a4ba1a03b3e65008fc5da76b70bf1690db4eae29c5f1badd03c
            5ccf2a55d705ddcd86d449511ceb7ec30bf12b1fa35b913f9f747a8afd1b130e
            94bff94effd01a91735ca1726acd0b197c4e5b03393697e126826fb6bbde8ecc
            1e08298516e2c9ed03ff3c1b7860f6de76d4cecd94c8119855ef5297ca67e9f3
            e7ff72b1e99785ca0a7e7720c5b36dc6d72cac9574c8cbbc2f801e23e56fd344
            b07f22154beba0f08ce8891e643ed995c94d9a69c9f1b5f499027a78572aeebd
            74d20cc39881c213ee770b1010e4bea718846977ae119f7a023ab58cca0ad752
            afe656bb3c17256a9f6e9bf19fdd5a38fc82bbe872c5539edb609ef4f79c203e
            bb140f2e583cb2ad15b4aa5b655016a8449277dbd477ef2c8d6c017db738b18d
            eb4a427d1923ce3ff262735779a418f20a282df920147beabe421ee5319d0568
            "#
        ),
    ),
    TestVector::xts_encrypt(
        &hex!("3141592653589793238462643383279502884197169399375105820974944592"),
        &hex!("2718281828459045235360287471352662497757247093699959574966967627"),
        &hex!("ff000000000000000000000000000000"),
        &hex!(r#"
                    000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
                    202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f
                    404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f
                    606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f
                    808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f
                    a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf
                    c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf
                    e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff
                    000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
                    202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f
                    404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f
                    606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f
                    808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f
                    a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf
                    c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf
                    e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff
        "#),
        &hex!(r#"
                    8d1d5a160589017ad54ca87344779650652504ad77bc398315d0133ed52d6520
                    77ab981b7eaf6e04197fbac41b94dcb89d668cfbdb130980f6e042f61e16a673
                    1f26542cf7f48f9a8f0915e480e0bae30512a36bffb70b44ea64ef33c6914df8
                    8ddc1159fe1d9f78c0c114e7bca3a157de5b0c4c965b54b4c60798b03fae6e26
                    a7bcec25c6e23521e8e7358de3a596b88aab2f8038a5069e9e95282b6a2e09c5
                    06fae21680151e91250ab33b439c4a7ed1c68ea9ce4b7808fc1ef94c99b53070
                    181e915dff53c85058f301bc0a81070f4cb45dd945a918cb60ceceaa72170ebb
                    c418177cba2589db1dfab4d5c3d209d227ce4abc7a86012b7199253cae28293b
                    ef9ba0534abf4232c7dc7d6c378978bb0e13d03397e9382c72711844d5390a99
                    9e3a04c2226c5205fc6d09826719b3c9e5020b4b41f75caee328132473959679
                    da17e973bc3cc7340e610a0d7e1a36cfbb925ed33f212f70110d9a1e951e06b2
                    1df72ea8c54ead4d0840d7b3ba5b6edbdbd002afe77cab7ca446a49759cea34d
                    52e2bb7a2f2d408b35a9b28e96ae760e7d521b7732ae7e5039cda260f6f6947f
                    31089287173682c93ea7d12032c1a8fe863d7ecf8e4c751a80375dec947e5ad6
                    cde9828d72598d5003c9d0ac91b083a3846fc25e067162ed5677117a276c46b4
                    29c11c5ff03db2b89e4e0de4c5c11d69b905a7d5908c956c92693216cfc6eb62
        "#)
    ),
    TestVector::xts_decrypt(
        &hex!("3141592653589793238462643383279502884197169399375105820974944592"),
        &hex!("2718281828459045235360287471352662497757247093699959574966967627"),
        &hex!("ff000000000000000000000000000000"),
        &hex!(r#"
                    8d1d5a160589017ad54ca87344779650652504ad77bc398315d0133ed52d6520
                    77ab981b7eaf6e04197fbac41b94dcb89d668cfbdb130980f6e042f61e16a673
                    1f26542cf7f48f9a8f0915e480e0bae30512a36bffb70b44ea64ef33c6914df8
                    8ddc1159fe1d9f78c0c114e7bca3a157de5b0c4c965b54b4c60798b03fae6e26
                    a7bcec25c6e23521e8e7358de3a596b88aab2f8038a5069e9e95282b6a2e09c5
                    06fae21680151e91250ab33b439c4a7ed1c68ea9ce4b7808fc1ef94c99b53070
                    181e915dff53c85058f301bc0a81070f4cb45dd945a918cb60ceceaa72170ebb
                    c418177cba2589db1dfab4d5c3d209d227ce4abc7a86012b7199253cae28293b
                    ef9ba0534abf4232c7dc7d6c378978bb0e13d03397e9382c72711844d5390a99
                    9e3a04c2226c5205fc6d09826719b3c9e5020b4b41f75caee328132473959679
                    da17e973bc3cc7340e610a0d7e1a36cfbb925ed33f212f70110d9a1e951e06b2
                    1df72ea8c54ead4d0840d7b3ba5b6edbdbd002afe77cab7ca446a49759cea34d
                    52e2bb7a2f2d408b35a9b28e96ae760e7d521b7732ae7e5039cda260f6f6947f
                    31089287173682c93ea7d12032c1a8fe863d7ecf8e4c751a80375dec947e5ad6
                    cde9828d72598d5003c9d0ac91b083a3846fc25e067162ed5677117a276c46b4
                    29c11c5ff03db2b89e4e0de4c5c11d69b905a7d5908c956c92693216cfc6eb62
        "#),
        &hex!(r#"
                    000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
                    202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f
                    404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f
                    606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f
                    808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f
                    a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf
                    c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf
                    e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff
                    000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
                    202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f
                    404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f
                    606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f
                    808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f
                    a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf
                    c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf
                    e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff
        "#),
    ),
];

fn run_tests() {
    let mut uart = UartType::new();

    writeln!(uart, "[*] Running {} tests", TEST_VECTORS.len()).ok();

    for (i, tv) in TEST_VECTORS.iter().enumerate() {
        let (mode, is_encrypt, input, expected_output) = match tv {
            TestVector::Ecb {
                key,
                input,
                output,
                encrypt,
            } => {
                let mode = AesMode::Ecb {
                    key: Key::try_from_slice(key).expect("invalid test vector key length"),
                };

                (mode, *encrypt, *input, *output)
            }
            TestVector::Cbc {
                key,
                iv,
                input,
                output,
                encrypt,
            } => {
                let mode = AesMode::Cbc {
                    key: Key::try_from_slice(key).expect("invalid test vector key length"),
                    iv: Iv::try_from_slice(iv).expect("invalid test vector IV length"),
                };

                (mode, *encrypt, *input, *output)
            }
            TestVector::Xts {
                key1,
                key2,
                tweak,
                input,
                output,
                encrypt,
            } => {
                let mode = AesMode::Xts {
                    key1: Key::try_from_slice(key1).expect("invalid test vector key1 length"),
                    key2: Key::try_from_slice(key2).expect("invalid test vector key2 length"),
                    tweak: (*tweak).try_into().expect("invalid tweak length"),
                };

                (mode, *encrypt, *input, *output)
            }
        };

        let (ch0, ch1) = init_dma();
        let (input, output) = unsafe {
            INPUT.0[..input.len()].copy_from_slice(input);
            OUTPUT.0.fill(0);
            (
                INPUT.0[..input.len()].as_ref(),
                OUTPUT.0[..expected_output.len()].as_mut(),
            )
        };

        let mut aes = Aes::default();

        if is_encrypt {
            aes.init_encrypt(mode);
        } else {
            aes.init_decrypt(mode);
        }

        aes.setup_for_dma(input.len());

        ch0.configure_peripheral_transfer(Aes::TX_DMA_CONFIG);
        ch1.configure_peripheral_transfer(Aes::RX_DMA_CONFIG);

        ch0.execute_transfer(
            input.as_ptr() as _,
            aes.dma_tx_addr() as _,
            input.len() / Aes::TX_DMA_CONFIG.data_width.byte_len(),
        );
        ch1.execute_transfer(
            aes.dma_rx_addr() as _,
            output.as_mut_ptr() as _,
            output.len() / Aes::RX_DMA_CONFIG.data_width.byte_len(),
        );

        wait_dma();
        // aes.process(input, output); // Uncomment for non-DMA processing

        if output != expected_output {
            writeln!(
                uart,
                "[!] Output does not match expected, vector #{}",
                i + 1
            )
            .ok();
            writeln!(uart, "Input was:").ok();
            print_hex(input);
            writeln!(uart, "Output was:").ok();
            print_hex(output);
            writeln!(uart, "Expected:").ok();
            print_hex(expected_output);
            return;
        }
    }

    writeln!(uart, "[+] All tests succeeded").ok();
}

fn init_dma() -> (XdmacChannel, XdmacChannel) {
    DMA_CH0_COMPLETE.store(false, SeqCst);
    DMA_CH1_COMPLETE.store(false, SeqCst);

    let xdmac = Xdmac::xdmac1();
    let ch0 = xdmac.channel(AES_TX_DMA_CHANNEL);
    ch0.disable();
    ch0.set_interrupt(true);
    ch0.set_bi_interrupt(true);

    let ch1 = xdmac.channel(AES_RX_DMA_CHANNEL);
    ch1.disable();
    ch1.set_interrupt(true);
    ch1.set_bi_interrupt(true);

    (ch0, ch1)
}

fn print_hex(slice: &[u8]) {
    let mut uart = UartType::new();

    const CHUNK_SIZE: usize = 16;
    let mut temp_buf: [u8; CHUNK_SIZE * 2] = [0; CHUNK_SIZE * 2];

    for chunk in slice.chunks_exact(CHUNK_SIZE) {
        temp_buf.fill(0);
        hex::encode_to_slice(chunk, &mut temp_buf).unwrap();
        let hash_str = core::str::from_utf8(&temp_buf).unwrap();
        writeln!(uart, "{}", hash_str).ok();
    }

    writeln!(uart).ok();
}

#[inline]
fn wait_dma() {
    while !DMA_CH1_COMPLETE.load(SeqCst) {
        armv7::asm::wfi();
    }

    DMA_CH1_COMPLETE.store(false, SeqCst);
}

// ----- Interrupt Handlers ------

#[no_mangle]
unsafe extern "C" fn aic_spurious_handler() {
    core::arch::asm!("bkpt");
}

#[no_mangle]
unsafe extern "C" fn aes_irq_handler() {
    let mut uart = UartType::new();
    writeln!(uart, "aes interrupt").ok();
}

#[no_mangle]
unsafe extern "C" fn xdmac_irq_handler() {
    let xdmac = Xdmac::xdmac1();
    let ch0 = xdmac.channel(AES_TX_DMA_CHANNEL);
    if ch0.interrupt_status() != 0 {
        DMA_CH0_COMPLETE.store(true, SeqCst);
    }

    let ch1 = xdmac.channel(AES_RX_DMA_CHANNEL);
    if ch1.interrupt_status() != 0 {
        // Signal to the main thread that the transfer is complete
        DMA_CH1_COMPLETE.store(true, SeqCst);
    }
}

#[inline(never)]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let mut console = Uart::<Uart1>::new();

    compiler_fence(SeqCst);
    writeln!(console, "{}", _info).ok();

    loop {
        unsafe {
            core::arch::asm!("bkpt");
        }
    }
}

// ----- Test Vector Definitions ------

enum TestVector {
    Ecb {
        key: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
        encrypt: bool,
    },

    Cbc {
        key: &'static [u8],
        iv: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
        encrypt: bool,
    },

    Xts {
        key1: &'static [u8],
        key2: &'static [u8],
        tweak: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
        encrypt: bool,
    },
}

impl TestVector {
    const fn ecb_encrypt(key: &'static [u8], input: &'static [u8], output: &'static [u8]) -> Self {
        Self::Ecb {
            key,
            input,
            output,
            encrypt: true,
        }
    }
    const fn ecb_decrypt(key: &'static [u8], input: &'static [u8], output: &'static [u8]) -> Self {
        Self::Ecb {
            key,
            input,
            output,
            encrypt: false,
        }
    }
    const fn cbc_encrypt(
        key: &'static [u8],
        iv: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
    ) -> Self {
        Self::Cbc {
            key,
            iv,
            input,
            output,
            encrypt: true,
        }
    }
    const fn cbc_decrypt(
        key: &'static [u8],
        iv: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
    ) -> Self {
        Self::Cbc {
            key,
            iv,
            input,
            output,
            encrypt: false,
        }
    }
    const fn xts_encrypt(
        key1: &'static [u8],
        key2: &'static [u8],
        tweak: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
    ) -> Self {
        Self::Xts {
            key1,
            key2,
            tweak,
            input,
            output,
            encrypt: true,
        }
    }
    const fn xts_decrypt(
        key1: &'static [u8],
        key2: &'static [u8],
        tweak: &'static [u8],
        input: &'static [u8],
        output: &'static [u8],
    ) -> Self {
        Self::Xts {
            key1,
            key2,
            tweak,
            input,
            output,
            encrypt: false,
        }
    }
}
