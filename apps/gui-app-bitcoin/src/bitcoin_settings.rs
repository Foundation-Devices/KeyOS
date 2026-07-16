// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint_keyos_platform::{
    slint::{ComponentHandle, ModelRc, VecModel},
    StoredValue,
};

use crate::{state::AppState, DisplayAmount, ImportPolicy, Settings, ShowFiatValue};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct BitcoinSettings {
    pub exchange_rate: ExchangeRate,
    pub display_amount: DisplayAmount,
    pub show_fiat_value: ShowFiatValue,
    pub import_policy: ImportPolicy,
    pub show_passphrase_warning: bool,
}

impl Default for BitcoinSettings {
    fn default() -> Self {
        Self {
            exchange_rate: ExchangeRate { currency_code: "USD".into(), rate: 100000.0 },
            display_amount: DisplayAmount::Auto,
            show_fiat_value: ShowFiatValue::Disabled,
            import_policy: ImportPolicy::AskToImport,
            show_passphrase_warning: true,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExchangeRate {
    pub currency_code: String,
    pub rate: f32,
}

impl From<quantum_link::foundation_api::fx::ExchangeRate> for ExchangeRate {
    fn from(exchange_rate: quantum_link::foundation_api::fx::ExchangeRate) -> Self {
        ExchangeRate { currency_code: exchange_rate.currency_code, rate: exchange_rate.rate }
    }
}

pub fn init_settings(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    {
        let state = state.borrow();
        let settings = &state.settings;
        ui.global::<Settings>().set_display_amount(settings.display_amount);
        ui.global::<Settings>().set_show_fiat_value(settings.show_fiat_value);
        ui.global::<Settings>().set_all_currencies(filter_search(""));
        ui.global::<Settings>().set_import_policy(settings.import_policy);
        ui.global::<Settings>().set_show_passphrase_warning(settings.show_passphrase_warning);
    }

    ui.global::<Settings>().on_set_display_amount(move |amount| {
        let mut state = state.borrow_mut();
        let ui = state.ui();
        state.settings.guard().display_amount = amount;
        ui.global::<Settings>().set_display_amount(amount);
    });

    ui.global::<Settings>().on_set_show_fiat_value(move |show_fiat_value| {
        {
            let mut state = state.borrow_mut();
            let ui = state.ui();
            state.settings.guard().show_fiat_value = show_fiat_value;
            ui.global::<Settings>().set_show_fiat_value(show_fiat_value);
        }
        AppState::publish_fiat_preference(state);
    });

    // search-show-fiat-value
    ui.global::<Settings>().on_search_show_fiat_value({
        move |search| {
            let ui = state.borrow().ui();
            ui.global::<Settings>().set_all_currencies(filter_search(&search));
        }
    });

    ui.global::<Settings>().on_fiat_value_symbol(|value| value.symbol().into());
    ui.global::<Settings>().on_fiat_value_name(|value| value.name().into());
    ui.global::<Settings>().on_fiat_value_code(|value| value.code().into());

    ui.global::<Settings>().on_set_import_policy(move |import_policy| {
        let mut state = state.borrow_mut();
        let ui = state.ui();
        state.settings.guard().import_policy = import_policy;
        ui.global::<Settings>().set_import_policy(import_policy);
    });

    ui.global::<Settings>().on_set_show_passphrase_warning(move |show| {
        let mut state = state.borrow_mut();
        let ui = state.ui();
        state.settings.guard().show_passphrase_warning = show;
        ui.global::<Settings>().set_show_passphrase_warning(show);
    });
}

fn filter_search(text: &str) -> ModelRc<ShowFiatValue> {
    // index starts at 1 to skip disabled option
    let values = FiatValueIter { index: 1 }
        .filter(move |fiat_value| {
            let name = fiat_value.name().to_lowercase();
            let code = fiat_value.code().to_lowercase();
            name.contains(text) || code.contains(text)
        })
        .collect::<VecModel<_>>();

    ModelRc::new(values)
}

impl ShowFiatValue {
    fn name(&self) -> &'static str {
        match self {
            ShowFiatValue::Disabled => "",
            // Major currencies
            ShowFiatValue::USD => "United States Dollar",
            ShowFiatValue::EUR => "Euro",
            ShowFiatValue::GBP => "British Pound Sterling",
            ShowFiatValue::JPY => "Japanese Yen",
            ShowFiatValue::AUD => "Australian Dollar",
            ShowFiatValue::CHF => "Swiss Franc",
            ShowFiatValue::CAD => "Canadian Dollar",
            ShowFiatValue::INR => "Indian Rupee",
            ShowFiatValue::AED => "United Arab Emirates Dirham",
            // Rest in alphabetical order
            ShowFiatValue::AFN => "Afghan Afghani",
            ShowFiatValue::ALL => "Albanian Lek",
            ShowFiatValue::AMD => "Armenian Dram",
            ShowFiatValue::ANG => "Netherlands Antillean Guilder",
            ShowFiatValue::AOA => "Angolan Kwanza",
            ShowFiatValue::ARS => "Argentine Peso",
            ShowFiatValue::AWG => "Aruban Florin",
            ShowFiatValue::AZN => "Azerbaijani Manat",
            ShowFiatValue::BAM => "Bosnia and Herzegovina Convertible Mark",
            ShowFiatValue::BBD => "Barbadian Dollar",
            ShowFiatValue::BDT => "Bangladeshi Taka",
            ShowFiatValue::BGN => "Bulgarian Lev",
            ShowFiatValue::BHD => "Bahraini Dinar",
            ShowFiatValue::BIF => "Burundian Franc",
            ShowFiatValue::BMD => "Bermudian Dollar",
            ShowFiatValue::BND => "Brunei Dollar",
            ShowFiatValue::BOB => "Bolivian Boliviano",
            ShowFiatValue::BRL => "Brazilian Real",
            ShowFiatValue::BSD => "Bahamian Dollar",
            ShowFiatValue::BTN => "Bhutanese Ngultrum",
            ShowFiatValue::BWP => "Botswana Pula",
            ShowFiatValue::BYN => "Belarusian Ruble",
            ShowFiatValue::BZD => "Belize Dollar",
            ShowFiatValue::CDF => "Congolese Franc",
            ShowFiatValue::CLF => "Unidad de Fomento",
            ShowFiatValue::CLP => "Chilean Peso",
            ShowFiatValue::CNH => "Chinese Yuan Renminbi",
            ShowFiatValue::CNY => "Chinese Yuan",
            ShowFiatValue::COP => "Colombian Peso",
            ShowFiatValue::CRC => "Costa Rican Colón",
            ShowFiatValue::CUC => "Cuban Convertible Peso",
            ShowFiatValue::CUP => "Cuban Peso",
            ShowFiatValue::CVE => "Cape Verdean Escudo",
            ShowFiatValue::CZK => "Czech Koruna",
            ShowFiatValue::DJF => "Djiboutian Franc",
            ShowFiatValue::DKK => "Danish Krone",
            ShowFiatValue::DOP => "Dominican Peso",
            ShowFiatValue::DZD => "Algerian Dinar",
            ShowFiatValue::EGP => "Egyptian Pound",
            ShowFiatValue::ERN => "Eritrean Nakfa",
            ShowFiatValue::ETB => "Ethiopian Birr",
            ShowFiatValue::FJD => "Fijian Dollar",
            ShowFiatValue::FKP => "Falkland Islands Pound",
            ShowFiatValue::GEL => "Georgian Lari",
            ShowFiatValue::GGP => "Guernsey Pound",
            ShowFiatValue::GHS => "Ghanaian Cedi",
            ShowFiatValue::GIP => "Gibraltar Pound",
            ShowFiatValue::GMD => "Gambian Dalasi",
            ShowFiatValue::GNF => "Guinean Franc",
            ShowFiatValue::GTQ => "Guatemalan Quetzal",
            ShowFiatValue::GYD => "Guyanese Dollar",
            ShowFiatValue::HKD => "Hong Kong Dollar",
            ShowFiatValue::HNL => "Honduran Lempira",
            ShowFiatValue::HRK => "Croatian Kuna",
            ShowFiatValue::HTG => "Haitian Gourde",
            ShowFiatValue::HUF => "Hungarian Forint",
            ShowFiatValue::IDR => "Indonesian Rupiah",
            ShowFiatValue::ILS => "Israeli New Shekel",
            ShowFiatValue::IMP => "Isle of Man Pound",
            ShowFiatValue::IQD => "Iraqi Dinar",
            ShowFiatValue::IRR => "Iranian Rial",
            ShowFiatValue::ISK => "Icelandic Króna",
            ShowFiatValue::JEP => "Jersey Pound",
            ShowFiatValue::JMD => "Jamaican Dollar",
            ShowFiatValue::JOD => "Jordanian Dinar",
            ShowFiatValue::KES => "Kenyan Shilling",
            ShowFiatValue::KGS => "Kyrgyzstani Som",
            ShowFiatValue::KHR => "Cambodian Riel",
            ShowFiatValue::KMF => "Comorian Franc",
            ShowFiatValue::KPW => "North Korean Won",
            ShowFiatValue::KRW => "South Korean Won",
            ShowFiatValue::KWD => "Kuwaiti Dinar",
            ShowFiatValue::KYD => "Cayman Islands Dollar",
            ShowFiatValue::KZT => "Kazakhstani Tenge",
            ShowFiatValue::LAK => "Lao Kip",
            ShowFiatValue::LBP => "Lebanese Pound",
            ShowFiatValue::LKR => "Sri Lankan Rupee",
            ShowFiatValue::LRD => "Liberian Dollar",
            ShowFiatValue::LSL => "Lesotho Loti",
            ShowFiatValue::LYD => "Libyan Dinar",
            ShowFiatValue::MAD => "Moroccan Dirham",
            ShowFiatValue::MDL => "Moldovan Leu",
            ShowFiatValue::MGA => "Malagasy Ariary",
            ShowFiatValue::MKD => "North Macedonian Denar",
            ShowFiatValue::MMK => "Myanmar Kyat",
            ShowFiatValue::MNT => "Mongolian Tögrög",
            ShowFiatValue::MOP => "Macanese Pataca",
            ShowFiatValue::MUR => "Mauritian Rupee",
            ShowFiatValue::MVR => "Maldivian Rufiyaa",
            ShowFiatValue::MWK => "Malawian Kwacha",
            ShowFiatValue::MXN => "Mexican Peso",
            ShowFiatValue::MYR => "Malaysian Ringgit",
            ShowFiatValue::MZN => "Mozambican Metical",
            ShowFiatValue::NAD => "Namibian Dollar",
            ShowFiatValue::NGN => "Nigerian Naira",
            ShowFiatValue::NIO => "Nicaraguan Córdoba",
            ShowFiatValue::NOK => "Norwegian Krone",
            ShowFiatValue::NPR => "Nepalese Rupee",
            ShowFiatValue::NZD => "New Zealand Dollar",
            ShowFiatValue::OMR => "Omani Rial",
            ShowFiatValue::PAB => "Panamanian Balboa",
            ShowFiatValue::PEN => "Peruvian Sol",
            ShowFiatValue::PGK => "Papua New Guinean Kina",
            ShowFiatValue::PHP => "Philippine Peso",
            ShowFiatValue::PKR => "Pakistani Rupee",
            ShowFiatValue::PLN => "Polish Zloty",
            ShowFiatValue::PYG => "Paraguayan Guarani",
            ShowFiatValue::QAR => "Qatari Rial",
            ShowFiatValue::RON => "Romanian Leu",
            ShowFiatValue::RSD => "Serbian Dinar",
            ShowFiatValue::RUB => "Russian Ruble",
            ShowFiatValue::RWF => "Rwandan Franc",
            ShowFiatValue::SAR => "Saudi Riyal",
            ShowFiatValue::SBD => "Solomon Islands Dollar",
            ShowFiatValue::SCR => "Seychellois Rupee",
            ShowFiatValue::SDG => "Sudanese Pound",
            ShowFiatValue::SEK => "Swedish Krona",
            ShowFiatValue::SGD => "Singapore Dollar",
            ShowFiatValue::SHP => "Saint Helena Pound",
            ShowFiatValue::SLL => "Sierra Leonean Leone",
            ShowFiatValue::SOS => "Somali Shilling",
            ShowFiatValue::SRD => "Surinamese Dollar",
            ShowFiatValue::SSP => "South Sudanese Pound",
            ShowFiatValue::STD => "São Tomé and Príncipe Dobra",
            ShowFiatValue::SVC => "Salvadoran Colón",
            ShowFiatValue::SYP => "Syrian Pound",
            ShowFiatValue::SZL => "Swazi Lilangeni",
            ShowFiatValue::THB => "Thai Baht",
            ShowFiatValue::TJS => "Tajikistani Somoni",
            ShowFiatValue::TMT => "Turkmenistani Manat",
            ShowFiatValue::TND => "Tunisian Dinar",
            ShowFiatValue::TOP => "Tongan Paʻanga",
            ShowFiatValue::TRY => "Turkish Lira",
            ShowFiatValue::TTD => "Trinidad and Tobago Dollar",
            ShowFiatValue::TWD => "New Taiwan Dollar",
            ShowFiatValue::TZS => "Tanzanian Shilling",
            ShowFiatValue::UAH => "Ukrainian Hryvnia",
            ShowFiatValue::UGX => "Ugandan Shilling",
            ShowFiatValue::UYU => "Uruguayan Peso",
            ShowFiatValue::UZS => "Uzbekistani Som",
            ShowFiatValue::VES => "Venezuelan Bolívar",
            ShowFiatValue::VND => "Vietnamese Dong",
            ShowFiatValue::VUV => "Vanuatu Vatu",
            ShowFiatValue::WST => "Samoan Tala",
            ShowFiatValue::XAG => "Silver Ounce",
            ShowFiatValue::XAU => "Gold Ounce",
            ShowFiatValue::XCD => "East Caribbean Dollar",
            ShowFiatValue::XDR => "Special Drawing Rights",
            ShowFiatValue::XPD => "Palladium Ounce",
            ShowFiatValue::XPF => "CFP Franc",
            ShowFiatValue::XPT => "Platinum Ounce",
            ShowFiatValue::YER => "Yemeni Rial",
            ShowFiatValue::ZAR => "South African Rand",
            ShowFiatValue::ZMW => "Zambian Kwacha",
            ShowFiatValue::ZWL => "Zimbabwean Dollar",
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            ShowFiatValue::Disabled => "",
            ShowFiatValue::USD => "USD",
            ShowFiatValue::EUR => "EUR",
            ShowFiatValue::GBP => "GBP",
            ShowFiatValue::JPY => "JPY",
            ShowFiatValue::AUD => "AUD",
            ShowFiatValue::CHF => "CHF",
            ShowFiatValue::CAD => "CAD",
            ShowFiatValue::INR => "INR",
            ShowFiatValue::AED => "AED",
            ShowFiatValue::AFN => "AFN",
            ShowFiatValue::ALL => "ALL",
            ShowFiatValue::AMD => "AMD",
            ShowFiatValue::ANG => "ANG",
            ShowFiatValue::AOA => "AOA",
            ShowFiatValue::ARS => "ARS",
            ShowFiatValue::AWG => "AWG",
            ShowFiatValue::AZN => "AZN",
            ShowFiatValue::BAM => "BAM",
            ShowFiatValue::BBD => "BBD",
            ShowFiatValue::BDT => "BDT",
            ShowFiatValue::BGN => "BGN",
            ShowFiatValue::BHD => "BHD",
            ShowFiatValue::BIF => "BIF",
            ShowFiatValue::BMD => "BMD",
            ShowFiatValue::BND => "BND",
            ShowFiatValue::BOB => "BOB",
            ShowFiatValue::BRL => "BRL",
            ShowFiatValue::BSD => "BSD",
            ShowFiatValue::BTN => "BTN",
            ShowFiatValue::BWP => "BWP",
            ShowFiatValue::BYN => "BYN",
            ShowFiatValue::BZD => "BZD",
            ShowFiatValue::CDF => "CDF",
            ShowFiatValue::CLF => "CLF",
            ShowFiatValue::CLP => "CLP",
            ShowFiatValue::CNH => "CNH",
            ShowFiatValue::CNY => "CNY",
            ShowFiatValue::COP => "COP",
            ShowFiatValue::CRC => "CRC",
            ShowFiatValue::CUC => "CUC",
            ShowFiatValue::CUP => "CUP",
            ShowFiatValue::CVE => "CVE",
            ShowFiatValue::CZK => "CZK",
            ShowFiatValue::DJF => "DJF",
            ShowFiatValue::DKK => "DKK",
            ShowFiatValue::DOP => "DOP",
            ShowFiatValue::DZD => "DZD",
            ShowFiatValue::EGP => "EGP",
            ShowFiatValue::ERN => "ERN",
            ShowFiatValue::ETB => "ETB",
            ShowFiatValue::FJD => "FJD",
            ShowFiatValue::FKP => "FKP",
            ShowFiatValue::GEL => "GEL",
            ShowFiatValue::GGP => "GGP",
            ShowFiatValue::GHS => "GHS",
            ShowFiatValue::GIP => "GIP",
            ShowFiatValue::GMD => "GMD",
            ShowFiatValue::GNF => "GNF",
            ShowFiatValue::GTQ => "GTQ",
            ShowFiatValue::GYD => "GYD",
            ShowFiatValue::HKD => "HKD",
            ShowFiatValue::HNL => "HNL",
            ShowFiatValue::HRK => "HRK",
            ShowFiatValue::HTG => "HTG",
            ShowFiatValue::HUF => "HUF",
            ShowFiatValue::IDR => "IDR",
            ShowFiatValue::ILS => "ILS",
            ShowFiatValue::IMP => "IMP",
            ShowFiatValue::IQD => "IQD",
            ShowFiatValue::IRR => "IRR",
            ShowFiatValue::ISK => "ISK",
            ShowFiatValue::JEP => "JEP",
            ShowFiatValue::JMD => "JMD",
            ShowFiatValue::JOD => "JOD",
            ShowFiatValue::KES => "KES",
            ShowFiatValue::KGS => "KGS",
            ShowFiatValue::KHR => "KHR",
            ShowFiatValue::KMF => "KMF",
            ShowFiatValue::KPW => "KPW",
            ShowFiatValue::KRW => "KRW",
            ShowFiatValue::KWD => "KWD",
            ShowFiatValue::KYD => "KYD",
            ShowFiatValue::KZT => "KZT",
            ShowFiatValue::LAK => "LAK",
            ShowFiatValue::LBP => "LBP",
            ShowFiatValue::LKR => "LKR",
            ShowFiatValue::LRD => "LRD",
            ShowFiatValue::LSL => "LSL",
            ShowFiatValue::LYD => "LYD",
            ShowFiatValue::MAD => "MAD",
            ShowFiatValue::MDL => "MDL",
            ShowFiatValue::MGA => "MGA",
            ShowFiatValue::MKD => "MKD",
            ShowFiatValue::MMK => "MMK",
            ShowFiatValue::MNT => "MNT",
            ShowFiatValue::MOP => "MOP",
            ShowFiatValue::MUR => "MUR",
            ShowFiatValue::MVR => "MVR",
            ShowFiatValue::MWK => "MWK",
            ShowFiatValue::MXN => "MXN",
            ShowFiatValue::MYR => "MYR",
            ShowFiatValue::MZN => "MZN",
            ShowFiatValue::NAD => "NAD",
            ShowFiatValue::NGN => "NGN",
            ShowFiatValue::NIO => "NIO",
            ShowFiatValue::NOK => "NOK",
            ShowFiatValue::NPR => "NPR",
            ShowFiatValue::NZD => "NZD",
            ShowFiatValue::OMR => "OMR",
            ShowFiatValue::PAB => "PAB",
            ShowFiatValue::PEN => "PEN",
            ShowFiatValue::PGK => "PGK",
            ShowFiatValue::PHP => "PHP",
            ShowFiatValue::PKR => "PKR",
            ShowFiatValue::PLN => "PLN",
            ShowFiatValue::PYG => "PYG",
            ShowFiatValue::QAR => "QAR",
            ShowFiatValue::RON => "RON",
            ShowFiatValue::RSD => "RSD",
            ShowFiatValue::RUB => "RUB",
            ShowFiatValue::RWF => "RWF",
            ShowFiatValue::SAR => "SAR",
            ShowFiatValue::SBD => "SBD",
            ShowFiatValue::SCR => "SCR",
            ShowFiatValue::SDG => "SDG",
            ShowFiatValue::SEK => "SEK",
            ShowFiatValue::SGD => "SGD",
            ShowFiatValue::SHP => "SHP",
            ShowFiatValue::SLL => "SLL",
            ShowFiatValue::SOS => "SOS",
            ShowFiatValue::SRD => "SRD",
            ShowFiatValue::SSP => "SSP",
            ShowFiatValue::STD => "STD",
            ShowFiatValue::SVC => "SVC",
            ShowFiatValue::SYP => "SYP",
            ShowFiatValue::SZL => "SZL",
            ShowFiatValue::THB => "THB",
            ShowFiatValue::TJS => "TJS",
            ShowFiatValue::TMT => "TMT",
            ShowFiatValue::TND => "TND",
            ShowFiatValue::TOP => "TOP",
            ShowFiatValue::TRY => "TRY",
            ShowFiatValue::TTD => "TTD",
            ShowFiatValue::TWD => "TWD",
            ShowFiatValue::TZS => "TZS",
            ShowFiatValue::UAH => "UAH",
            ShowFiatValue::UGX => "UGX",
            ShowFiatValue::UYU => "UYU",
            ShowFiatValue::UZS => "UZS",
            ShowFiatValue::VES => "VES",
            ShowFiatValue::VND => "VND",
            ShowFiatValue::VUV => "VUV",
            ShowFiatValue::WST => "WST",
            ShowFiatValue::XAG => "XAG",
            ShowFiatValue::XAU => "XAU",
            ShowFiatValue::XCD => "XCD",
            ShowFiatValue::XDR => "XDR",
            ShowFiatValue::XPD => "XPD",
            ShowFiatValue::XPF => "XPF",
            ShowFiatValue::XPT => "XPT",
            ShowFiatValue::YER => "YER",
            ShowFiatValue::ZAR => "ZAR",
            ShowFiatValue::ZMW => "ZMW",
            ShowFiatValue::ZWL => "ZWL",
        }
    }

    pub(crate) fn symbol(&self) -> &'static str { fiat_symbols::symbol_for_code(self.code()) }
}

pub struct FiatValueIter {
    index: u32,
}

impl Iterator for FiatValueIter {
    type Item = ShowFiatValue;

    fn next(&mut self) -> Option<Self::Item> {
        use num_traits::FromPrimitive;
        let fiat_value = ShowFiatValue::from_u32(self.index)?;
        self.index += 1;
        Some(fiat_value)
    }
}
