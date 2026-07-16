#![no_std]

const OVM7690_ADDRESS: u8 = 0x21;
const OVM7690_CHIP_ID: [u8; 2] = [0x7F, 0xA2];

#[derive(Debug)]
pub enum Ovm7690Error<I2C> {
    I2cError(I2C),
}

pub struct Ovm7690<I2C> {
    i2c: I2C,
}

impl<I2C: embedded_hal::i2c::I2c> Ovm7690<I2C> {
    pub fn new(i2c: I2C) -> Self { Self { i2c } }

    pub fn verify_chip_id(&mut self) -> Result<bool, Ovm7690Error<I2C::Error>> {
        let mut chip_id_buf = [0; 2];
        self.i2c
            .write_read(OVM7690_ADDRESS, &[Register::MIDH as u8], &mut chip_id_buf)
            .map_err(Ovm7690Error::I2cError)?;

        Ok(chip_id_buf == OVM7690_CHIP_ID)
    }

    pub fn init(&mut self) -> Result<(), Ovm7690Error<I2C::Error>> {
        for (reg, val) in INIT_SEQUENCE {
            self.write_reg(reg, val)?;
        }

        // Don't reset camera sensor timing when mode changes.
        let mut reg = self.read_reg(Register::REG6F)?;
        reg &= !(1 << 7);
        self.write_reg(Register::REG6F, reg)?;

        Ok(())
    }

    pub fn sw_reset(&mut self) -> Result<(), Ovm7690Error<I2C::Error>> {
        self.write_reg(Register::REG12, 0x80)?;

        Ok(())
    }

    pub fn enable(&mut self) -> Result<(), Ovm7690Error<I2C::Error>> {
        let mut reg = self.read_reg(Register::REG0E)?;
        reg &= !(1 << 3);
        self.write_reg(Register::REG0E, reg)?;

        Ok(())
    }

    pub fn disable(&mut self) -> Result<(), Ovm7690Error<I2C::Error>> {
        let mut reg = self.read_reg(Register::REG0E)?;
        reg |= 1 << 3;
        self.write_reg(Register::REG0E, reg)?;

        Ok(())
    }

    /// Set brightness (Ybright register). Range: 0-255, default: 0
    pub fn set_brightness(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        self.write_reg(Register::REGD3, value)
    }

    /// Get current brightness value
    pub fn brightness(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REGD3)
    }

    /// Set contrast (Ygain register). Range: 0-255, default: 0x28 (>0x20 increases contrast)
    pub fn set_contrast(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        self.write_reg(Register::REGD4, value)
    }

    /// Get current contrast value
    pub fn contrast(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REGD4)
    }

    /// Set saturation (Sat_u and Sat_v registers). Range: 0-255, default: 0x50
    pub fn set_saturation(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        self.write_reg(Register::REGD8, value)?;
        self.write_reg(Register::REGD9, value)
    }

    /// Get current saturation value (returns Sat_u)
    pub fn saturation(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REGD8)
    }

    /// Set sharpness magnitude (REGB6). Range: 0-31, default: 0x04
    /// Higher values = more edge enhancement (sharper image)
    /// Only effective when auto sharpness is disabled
    pub fn set_sharpness(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        // REGB6 bits[4:0] = sharpening magnitude
        let current = self.read_reg(Register::REGB6)?;
        let new_value = (current & 0xE0) | (value & 0x1F);
        self.write_reg(Register::REGB6, new_value)
    }

    /// Get current sharpness value
    pub fn sharpness(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REGB6).map(|v| v & 0x1F)
    }

    /// Set denoise magnitude (REGB5). Range: 0-255, default: 0x08
    /// Higher values = more aggressive noise reduction (may lose detail)
    /// Only effective when auto denoise is disabled
    pub fn set_denoise(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        // REGB5 bits[7:0] = de-noise magnitude
        self.write_reg(Register::REGB5, value)
    }

    /// Get current denoise value
    pub fn denoise(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REGB5)
    }

    /// Set auto edge/denoise controls (REGB4)
    /// Bit[5] = sharpening mode (0=Manual, 1=Auto)
    /// Bit[4] = de-noise mode (0=Manual, 1=Auto)
    pub fn set_auto_edge_denoise(&mut self, auto_sharpness: bool, auto_denoise: bool) -> Result<(), Ovm7690Error<I2C::Error>> {
        let current = self.read_reg(Register::REGB4)?;
        let mut value = current & !0x30; // Clear bits 4 and 5
        if auto_denoise {
            value |= 0x10; // Bit 4
        }
        if auto_sharpness {
            value |= 0x20; // Bit 5
        }
        self.write_reg(Register::REGB4, value)
    }

    /// Get auto edge/denoise settings
    /// Returns (auto_sharpness, auto_denoise)
    pub fn auto_edge_denoise(&mut self) -> Result<(bool, bool), Ovm7690Error<I2C::Error>> {
        let value = self.read_reg(Register::REGB4)?;
        Ok(((value & 0x20) != 0, (value & 0x10) != 0))
    }

    /// Set auto control features (REG13)
    /// Bits: 0 = AEC, 1 = AWB, 2 = AGC
    /// Other bits (5,7) are preserved from default 0xe0
    pub fn set_auto_controls(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        // Combine with base value 0xe0 (fast AEC, banding filter bits)
        let reg_value = 0xe0 | (value & 0x07);
        self.write_reg(Register::REG13, reg_value)
    }

    /// Get current auto control settings
    pub fn auto_controls(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REG13).map(|v| v & 0x07)
    }

    /// Set AGC ceiling (maximum gain)
    /// Values: 0=2x, 1=4x, 2=8x, 3=16x, 4=32x, 5=64x, 6=128x
    pub fn set_agc_ceiling(&mut self, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        // Read current REG14 value to preserve other bits
        let current = self.read_reg(Register::REG14)?;
        // Clear bits[6:4] and set new ceiling value
        let new_value = (current & 0x8F) | ((value & 0x07) << 4);
        self.write_reg(Register::REG14, new_value)
    }

    /// Get current AGC ceiling
    pub fn agc_ceiling(&mut self) -> Result<u8, Ovm7690Error<I2C::Error>> {
        self.read_reg(Register::REG14).map(|v| (v >> 4) & 0x07)
    }


    pub fn release_i2c(self) -> I2C { self.i2c }

    fn write_reg(&mut self, reg: Register, value: u8) -> Result<(), Ovm7690Error<I2C::Error>> {
        self.i2c.write_read(OVM7690_ADDRESS, &[reg as u8, value], &mut []).map_err(Ovm7690Error::I2cError)
    }

    pub fn read_reg(&mut self, reg: Register) -> Result<u8, Ovm7690Error<I2C::Error>> {
        let mut byte_buf = [0; 1];
        self.i2c.write_read(OVM7690_ADDRESS, &[reg as u8], &mut byte_buf).map_err(Ovm7690Error::I2cError)?;

        Ok(byte_buf[0])
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum Register {
    /* Camera registers */
    GAIN = 0x00,
    BGAIN = 0x01,
    RGAIN = 0x02,
    GGAIN = 0x03,
    YAVG = 0x04,
    BAVG = 0x05,
    RAVG = 0x06,
    /* 0x08 - 0x09 reserved */
    PIDH = 0x0A,
    PIDL = 0x0B,
    REG0C = 0x0C,
    REG0D = 0x0D,
    REG0E = 0x0E,
    AECH = 0x0F,
    AECL = 0x10,
    CLKRC = 0x11,
    REG12 = 0x12,
    REG13 = 0x13,
    REG14 = 0x14,
    REG15 = 0x15,
    REG16 = 0x16,
    HSTART = 0x17,
    HSIZE = 0x18,
    VSTART = 0x19,
    VSIZE = 0x1A,
    SHFT = 0x1B,
    MIDH = 0x1C,
    MIDL = 0x1D,
    REG1E = 0x1E,
    REG20 = 0x20,
    AECGM = 0x21,
    REG22 = 0x22,
    WPT = 0x24,
    BPT = 0x25,
    VPT = 0x26,
    REG27 = 0x27,
    REG28 = 0x28,
    PLL = 0x29,
    EXHCL = 0x2A,
    EXHCH = 0x2B,
    DmLn = 0x2C,
    ADVFL = 0x2D,
    ADVFH = 0x2E,
    SOC = 0x38,
    REG39 = 0x39,
    REG3E = 0x3E,
    REG3F = 0x3F,
    ANA1 = 0x48,
    PWC0 = 0x49,
    BD50ST = 0x50,
    BD60ST = 0x51,
    UVCTR0 = 0x5A,
    UVCTR1 = 0x5B,
    UVCTR2 = 0x5C,
    UVCTR3 = 0x5D,
    REG62 = 0x62,
    BLC8 = 0x68,
    BLCOUT = 0x6B,
    REG6F = 0x6F,
    REG80 = 0x80,
    REG81 = 0x81,
    REG82 = 0x82,
    REG8C = 0x8C,
    REG8D = 0x8D,
    REG8E = 0x8E,
    REG8F = 0x8F,
    REG90 = 0x90,
    REG91 = 0x91,
    REG92 = 0x92,
    REG93 = 0x93,
    REG94 = 0x94,
    REG95 = 0x95,
    REG96 = 0x96,
    REG97 = 0x97,
    REG98 = 0x98,
    REG99 = 0x99,
    REG9A = 0x9A,
    REG9B = 0x9B,
    REG9C = 0x9C,
    REG9D = 0x9D,
    REG9E = 0x9E,
    REG9F = 0x9F,

    REGA0 = 0xA0,
    REGA1 = 0xA1,
    REGA2 = 0xA2,

    LCC0 = 0x85,
    LCC1 = 0x86,
    LCC2 = 0x87,
    LCC3 = 0x88,
    LCC4 = 0x89,
    LCC5 = 0x8A,
    LCC6 = 0x8B,
    GAM1 = 0xA3,
    GAM2 = 0xA4,
    GAM3 = 0xA5,
    GAM4 = 0xA6,
    GAM5 = 0xA7,
    GAM6 = 0xA8,
    GAM7 = 0xA9,
    GAM8 = 0xAA,
    GAM9 = 0xAB,
    GAM10 = 0xAC,
    GAM11 = 0xAD,
    GAM12 = 0xAE,
    GAM13 = 0xAF,
    GAM14 = 0xB0,
    GAM15 = 0xB1,
    SLOPE = 0xB2,

    REGB4 = 0xB4,
    REGB5 = 0xB5,
    REGB6 = 0xB6,
    REGB7 = 0xB7,
    REGB8 = 0xB8,
    REGB9 = 0xB9,
    REGBA = 0xBA,
    REGBB = 0xBB,
    REGBC = 0xBC,
    REGBD = 0xBD,
    REGBE = 0xBE,
    REGBF = 0xBF,
    REGC0 = 0xC0,
    REGC1 = 0xC1,
    REGC2 = 0xC2,
    REGC3 = 0xC3,
    REGC4 = 0xC4,
    REGC5 = 0xC5,
    REGC6 = 0xC6,
    REGC7 = 0xC7,
    REGC8 = 0xC8,
    REGC9 = 0xC9,
    REGCA = 0xCA,
    REGCB = 0xCB,
    REGCC = 0xCC,
    REGCD = 0xCD,
    REGCE = 0xCE,
    REGCF = 0xCF,
    REGD0 = 0xD0,

    REGD2 = 0xD2,
    REGD3 = 0xD3,
    REGD4 = 0xD4,
    REGD5 = 0xD5,
    REGD6 = 0xD6,
    REGD7 = 0xD7,
    REGD8 = 0xD8,
    REGD9 = 0xD9,
    REGDA = 0xDA,
    REGDB = 0xDB,
    REGDC = 0xDC,
    REGDD = 0xDD,
    REGDE = 0xDE,
    REGDF = 0xDF,
    REGE0 = 0xE0,
    REGE1 = 0xE1,
}

const INIT_SEQUENCE: [(Register, u8); 99] = [
    // Sensor   : OVM7690
    (Register::REG0C, 0x06 | 1 << 6 | 1 << 7), // Enable output, mirror and flip
    (Register::REG81, 0xff), // Special Digital Effects enabled
    (Register::AECGM, 0x23),
    (Register::REG39, 0x80),
    (Register::REG1E, 0xb1),
    //===Fixed Gain (when AGC is disabled)===
    (Register::GAIN, 0x00),  // Global gain - 0x00 = 1x gain
    (Register::RGAIN, 0x40), // Red gain - balanced (default)
    (Register::GGAIN, 0x40), // Green gain - balanced (default)
    (Register::BGAIN, 0x40), // Blue gain - balanced (default)
    //===Fixed Exposure (when AEC is disabled)===
    (Register::AECH, 0x80),  // Exposure high byte (increased for brighter image)
    (Register::AECL, 0x00),  // Exposure low byte
    //===Format===
    (Register::REG12, 0x06), // RGB + RGB565
    (Register::REG82, 0x03), // YUV422[2]
    (Register::REGD0, 0x48),
    (Register::REG80, 0xff),
    (Register::REG3E, 0x70), // PCLK gated + PCLK for RGB/YUV format
    //===Resolution===
    (Register::HSIZE, 0xa4),
    (Register::VSIZE, 0xf6),
    //===Position===
    (Register::HSTART, 0x69), // h
    (Register::VSTART, 0x0e), // v
    //===Size===
    (Register::REGC8, 0x01), // input horiz
    (Register::REGC9, 0xE0), // ^- 480
    (Register::REGCC, 0x01), // output horiz
    (Register::REGCD, 0xE0), // ^- 480
    //===Lens Correction==
    (Register::LCC0, 0x90),
    (Register::LCC1, 0x00),
    (Register::LCC2, 0x00),
    (Register::LCC3, 0x10),
    (Register::LCC4, 0x30),
    (Register::LCC5, 0x29),
    (Register::LCC6, 0x26),
    //====Color Matrix====
    (Register::REGBB, 0x80),
    (Register::REGBC, 0x62),
    (Register::REGBD, 0x1e),
    (Register::REGBE, 0x26),
    (Register::REGBF, 0x7b),
    (Register::REGC0, 0xac),
    (Register::REGC1, 0x1e),
    //===Edge + Denoise====
    (Register::REGB7, 0x05),
    (Register::REGB8, 0x09),
    (Register::REGB9, 0x00),
    (Register::REGBA, 0x18),
    //===UVAdjust====
    (Register::UVCTR0, 0x4A),
    (Register::UVCTR1, 0x9F),
    (Register::UVCTR2, 0x48),
    (Register::UVCTR3, 0x32),
    //====AEC/AGC target====
    (Register::WPT, 0x78),
    (Register::BPT, 0x68),
    (Register::VPT, 0xb3),
    //====Gamma====
    (Register::GAM1, 0x0b),
    (Register::GAM2, 0x15),
    (Register::GAM3, 0x2a),
    (Register::GAM4, 0x51),
    (Register::GAM5, 0x63),
    (Register::GAM6, 0x74),
    (Register::GAM7, 0x83),
    (Register::GAM8, 0x91),
    (Register::GAM9, 0x9e),
    (Register::GAM10, 0xaa),
    (Register::GAM11, 0xbe),
    (Register::GAM12, 0xce),
    (Register::GAM13, 0xe5),
    (Register::GAM14, 0xf3),
    (Register::GAM15, 0xfb),
    (Register::SLOPE, 0x06),
    //===AWB===
    //==Advanced==
    (Register::REG8C, 0x5d),
    (Register::REG8D, 0x11),
    (Register::REG8E, 0x12),
    (Register::REG8F, 0x11),
    (Register::REG90, 0x50),
    (Register::REG91, 0x22),
    (Register::REG92, 0xd1),
    (Register::REG93, 0xa7),
    (Register::REG94, 0x23),
    (Register::REG95, 0x3b),
    (Register::REG96, 0xff),
    (Register::REG97, 0x00),
    (Register::REG98, 0x4a),
    (Register::REG99, 0x46),
    (Register::REG9A, 0x3d),
    (Register::REG9B, 0x3a),
    (Register::REG9C, 0xf0),
    (Register::REG9D, 0xf0),
    (Register::REG9E, 0xf0),
    (Register::REG9F, 0xff),
    (Register::REGA0, 0x56),
    (Register::REGA1, 0x55),
    (Register::REGA2, 0x13),
    //====SDE (Special Digital Effects)====
    (Register::REGD2, 0x06), // Enable Sat_en + Cont_en (bits 1,2)
    (Register::REGD3, 0x00), // Ybright - default 0x00
    (Register::REGD4, 0x28), // Ygain (contrast) - >0x20 increases contrast (default 0x20)
    (Register::REGD5, 0x00), // Yoffset - default 0x00
    (Register::REGD8, 0x50), // Sat_u: >0x40 increases U saturation (default 0x40)
    (Register::REGD9, 0x50), // Sat_v: >0x40 increases V saturation (default 0x40)
    //==General Control==
    (Register::BD50ST, 0x9a),
    (Register::BD60ST, 0x80),
    (Register::REG14, 0x19), // AGC max gain: 4x (8x default)
    (Register::REG13, 0xe7),
    (Register::CLKRC, 0x40), // Changed from 0 - we use an external oscillator
];
