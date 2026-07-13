// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::rc::Rc;

use itertools::Itertools;
use slint::{Color as SlintColor, Model, ModelRc, SharedString, ToSharedString, VecModel};

pub fn color_to_hex(color: SlintColor) -> SharedString {
    format!("#{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue()).into()
}

pub fn percent_to_string(value: f32, precision: i32) -> SharedString {
    format!("{:.1$}%", value, precision as usize).into()
}

pub fn color_from_hsl(hue: f32, saturation: f32, value: f32, alpha: f32) -> SlintColor {
    SlintColor::from_hsva(hue, saturation, value, alpha)
}

pub fn color_from_rgb(red: i32, green: i32, blue: i32, alpha: i32) -> SlintColor {
    SlintColor::from_argb_u8(alpha as u8, red as u8, green as u8, blue as u8)
}

pub fn get_hsv(c: SlintColor) -> ModelRc<f32> {
    let mut x = Vec::<f32>::new();
    let c = c.to_hsva();
    x.push(c.hue);
    x.push(c.saturation);
    x.push(c.value);
    x.push(c.alpha);
    ModelRc::from(Rc::new(VecModel::from(x)))
}

pub fn get_rgb(c: SlintColor) -> ModelRc<i32> {
    let mut x = Vec::<i32>::new();
    x.push(c.red() as i32);
    x.push(c.green() as i32);
    x.push(c.blue() as i32);
    x.push(c.alpha() as i32);
    ModelRc::from(Rc::new(VecModel::from(x)))
}

pub fn index_of(list: ModelRc<SharedString>, item: SharedString) -> i32 {
    let x = list.iter().position(|s| s == item);
    match x {
        Some(index) => index as i32,
        None => -1,
    }
}

pub fn join(list: ModelRc<SharedString>, delimeter: SharedString) -> SharedString {
    list.iter().join(&delimeter).into()
}

pub fn split(source: SharedString, delimeter: SharedString) -> ModelRc<SharedString> {
    let x = source.to_string();
    let d = delimeter.to_string();
    let x = x.split(&d);
    let x = x.map(|s| SharedString::from(s)).collect::<Vec<SharedString>>();
    let the_model: Rc<VecModel<SharedString>> = Rc::new(VecModel::from(x));
    ModelRc::from(the_model)
}

pub fn split_by_length(source: SharedString, length: i32) -> ModelRc<SharedString> {
    let s = source.to_string();
    let length = length as usize;
    let chunks: Vec<SharedString> = s
        .chars()
        .collect::<Vec<char>>()
        .chunks(length)
        .map(|chunk| chunk.iter().collect::<String>().into())
        .collect();
    let the_model: Rc<VecModel<SharedString>> = Rc::new(VecModel::from(chunks));
    ModelRc::from(the_model)
}

// filter out all except numbers and period(s), then cut all after 2nd periiod if any
pub fn string_to_percent(s: SharedString) -> f32 {
    let s = s.chars().filter(|&c| "0123456789.".contains(c)).collect::<String>();
    let sa = s.match_indices('.').nth(1).map(|(index, _)| s.split_at(index));
    let s = match sa {
        Some((x, _)) => x.to_string(),
        None => s,
    };
    let x = s.parse::<f32>();
    match x {
        Ok(x) => x,
        _ => 0.0,
    }
}

pub fn fill_array(size: i32, fill: i32, a: i32, b: i32) -> ModelRc<i32> {
    let mut v: Vec<i32> = Vec::with_capacity(size as usize);
    for i in 0..size {
        let x = if i < fill { a } else { b };
        v.push(x);
    }
    let the_model: Rc<VecModel<i32>> = Rc::new(VecModel::from(v));
    slint::ModelRc::from(the_model)
}

pub fn shorten_string(src: SharedString, max_len: i32, elide: bool) -> SharedString {
    let max_len = max_len as usize;
    let mut s = src.to_string();
    if s.chars().count() > max_len {
        let end = s.char_indices().nth(max_len).map_or(s.len(), |(i, _)| i);
        s.truncate(end);

        if elide {
            s.pop();
            s.push('…');
        }
    }
    s.into()
}

pub fn check_bounds(num: SharedString, minimum: SharedString, maximum: SharedString) -> SharedString {
    if num.is_empty() {
        return num;
    }

    let num_val = num.trim().parse::<i64>().unwrap_or(0);
    let mut bounded_val = num_val;

    let min_result = minimum.trim().parse::<i64>();
    let max_result = maximum.trim().parse::<i64>();

    if let (Ok(min_val), Ok(max_val)) = (&min_result, &max_result) {
        if min_val > max_val {
            return num;
        }
    }

    if let Ok(min_val) = min_result {
        bounded_val = bounded_val.max(min_val);
    }

    if let Ok(max_val) = max_result {
        bounded_val = bounded_val.min(max_val);
    }

    bounded_val.to_shared_string()
}

pub fn is_in_bounds(num: SharedString, minimum: SharedString, maximum: SharedString) -> bool {
    if num.is_empty() {
        return false;
    }

    let num_val = match num.trim().parse::<i64>() {
        Ok(num_val) => num_val,
        Err(_) => return false,
    };

    let min_result = minimum.trim().parse::<i64>();
    let max_result = maximum.trim().parse::<i64>();

    if let (Ok(min_val), Ok(max_val)) = (&min_result, &max_result) {
        if min_val > max_val {
            return false;
        }
    }

    if let Ok(min_val) = min_result {
        if num_val < min_val {
            return false;
        }
    }

    if let Ok(max_val) = max_result {
        if num_val > max_val {
            return false;
        }
    }

    true
}

pub fn exceeds_maximum(num: SharedString, maximum: SharedString) -> bool {
    if num.is_empty() {
        return false;
    }

    let num_val = match num.trim().parse::<i64>() {
        Ok(num_val) => num_val,
        Err(_) => return true,
    };

    match maximum.trim().parse::<i64>() {
        Ok(max_val) => num_val > max_val,
        Err(_) => false,
    }
}

pub fn decrement_string(num: SharedString) -> SharedString {
    let num_val = num.trim().parse::<i64>().unwrap_or(0);
    let decremented = num_val.saturating_sub(1);
    decremented.to_shared_string()
}

pub fn increment_string(num: SharedString) -> SharedString {
    let num_val = num.trim().parse::<i64>().unwrap_or(0);
    let incremented = num_val.saturating_add(1);
    incremented.to_shared_string()
}

pub fn filter_special_characters(text: SharedString) -> SharedString {
    let forbidden = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let filtered: String = text
        .as_str()
        .chars()
        .filter_map(|c| if c.is_control() || forbidden.contains(&c) { None } else { Some(c) })
        .collect();

    filtered.into()
}
