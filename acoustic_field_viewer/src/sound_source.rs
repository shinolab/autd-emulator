/*
 * File: sound_source.rs
 * Project: src
 * Created Date: 27/04/2020
 * Author: Shun Suzuki
 * -----
 * Last Modified: 06/07/2021
 * Modified By: Shun Suzuki (suzuki@hapis.k.u-tokyo.ac.jp)
 * -----
 * Copyright (c) 2020 Hapis Lab. All rights reserved.
 *
 */

use crate::Vector3;

#[derive(Debug, Clone, Copy)]
pub struct SoundSource {
    pub pos: Vector3,
    pub dir: Vector3,
    pub amp: f32,
    pub phase: f32,
}

impl SoundSource {
    pub fn new(pos: Vector3, dir: Vector3, amp: f32, phase: f32) -> SoundSource {
        SoundSource {
            pos,
            dir,
            amp,
            phase,
        }
    }
}
