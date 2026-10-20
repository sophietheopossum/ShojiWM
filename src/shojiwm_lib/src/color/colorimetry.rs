//! Gamut conversion matrix derivation (RGB → CIE XYZ → RGB) and the SMPTE
//! ST 2084 (PQ) reference curves.
//!
//! The GPU encode pass (`backend/output_encode.frag`) hardcodes the
//! BT.709→BT.2020 matrix and PQ constants for GLSL ES 1.00 compatibility;
//! the unit tests here re-derive them from first principles so a typo in
//! either place fails CI instead of shipping subtly wrong color.

use super::ColorPrimaries;
use super::primaries::PrimariesChromaticities;

pub type Mat3 = [[f64; 3]; 3];

/// RGB → CIE XYZ for the given primaries, white point normalized to Y = 1.
fn rgb_to_xyz(
    chroma: PrimariesChromaticities
) -> Mat3 {
    // Columns of the un-scaled matrix are the primaries' XYZ coordinates.
    let xyz = |x: f64, y: f64| [x / y, 1.0, (1.0 - x - y) / y];
    let red = xyz(
        chroma.red.x as f64,
        chroma.red.y as f64
    );
    let green = xyz(
        chroma.green.x as f64,
        chroma.green.y as f64
    );
    let blue = xyz(
        chroma.blue.x as f64,
        chroma.blue.y as f64
    );
    let white = xyz(
        chroma.white.x as f64,
        chroma.white.y as f64
    );

    // Solve for the per-primary scales that make RGB(1,1,1) hit the white
    // point: S = M⁻¹ · W.
    let unscaled: Mat3 = [
        [red[0], green[0], blue[0]],
        [red[1], green[1], blue[1]],
        [red[2], green[2], blue[2]],
    ];
    let scales = mat3_mul_vec(
        &invert(
            &unscaled
        ),
        &white
    );
    [
        [red[0] * scales[0], green[0] * scales[1], blue[0] * scales[2]],
        [red[1] * scales[0], green[1] * scales[1], blue[1] * scales[2]],
        [red[2] * scales[0], green[2] * scales[1], blue[2] * scales[2]],
    ]
}

fn invert(
    m: &Mat3
) -> Mat3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv_det = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ]
}

fn mat3_mul(
    a: &Mat3,
    b: &Mat3
) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (
        row_index,
        row
    ) in out.iter_mut().enumerate() {
        for (
            col_index,
            cell
        ) in row.iter_mut().enumerate() {
            *cell = (0..3)
                .map(|k| a[row_index][k] * b[k][col_index])
                .sum::<f64>();
        }
    }
    out
}

fn mat3_mul_vec(
    m: &Mat3,
    v: &[f64; 3]
) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Row-major linear-light conversion matrix between two named gamuts
/// (same D65 white point on both sides, so no chromatic adaptation step).
pub fn gamut_conversion_matrix(
    from: ColorPrimaries,
    to: ColorPrimaries
) -> Mat3 {
    mat3_mul(
        &invert(
            &rgb_to_xyz(
                to.chromaticities()
            )
        ),
        &rgb_to_xyz(
            from.chromaticities()
        ),
    )
}

/// Row-major linear-light conversion between arbitrary primaries. No
/// chromatic adaptation: each side keeps its own white point, so RGB white
/// maps to the source's white. That is what showing content "in the panel's
/// native gamut" means — exactly what the panel does with it in SDR mode.
pub fn chromaticity_conversion_matrix(
    from: PrimariesChromaticities,
    to: PrimariesChromaticities,
) -> Mat3 {
    mat3_mul(&invert(&rgb_to_xyz(to)), &rgb_to_xyz(from))
}

/// `m` as the column-major array GLSL `mat3` uniforms take.
pub fn to_gl_mat3(m: &Mat3) -> [f32; 9] {
    [
        m[0][0] as f32, m[1][0] as f32, m[2][0] as f32,
        m[0][1] as f32, m[1][1] as f32, m[2][1] as f32,
        m[0][2] as f32, m[1][2] as f32, m[2][2] as f32,
    ]
}

// SMPTE ST 2084 constants.
const PQ_M1: f64 = 1305.0 / 8192.0;
const PQ_M2: f64 = 2523.0 / 32.0;
const PQ_C1: f64 = 107.0 / 128.0;
const PQ_C2: f64 = 2413.0 / 128.0;
const PQ_C3: f64 = 2392.0 / 128.0;

/// PQ inverse EOTF: absolute luminance (cd/m²) → PQ signal in [0, 1].
pub fn pq_inverse_eotf(
    nits: f64
) -> f64 {
    let y = (nits / 10000.0).clamp(
        0.0,
        1.0
    );
    let ym = y.powf(
        PQ_M1
    );
    ((PQ_C1 + PQ_C2 * ym) / (1.0 + PQ_C3 * ym)).powf(PQ_M2)
}

/// PQ EOTF: PQ signal in [0, 1] → absolute luminance (cd/m²).
pub fn pq_eotf(
    signal: f64
) -> f64 {
    let e = signal.clamp(
        0.0,
        1.0
    ).powf(1.0 / PQ_M2);
    let y = ((e - PQ_C1).max(0.0)) / (PQ_C2 - PQ_C3 * e);
    10000.0 * y.powf(1.0 / PQ_M1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matrix hardcoded in `backend/output_encode.frag` (row-major
    /// here; the shader stores it column-major for GLSL).
    const SHADER_BT709_TO_BT2020: Mat3 = [
        [0.627404, 0.329283, 0.043313],
        [0.069097, 0.919540, 0.011362],
        [0.016391, 0.088013, 0.895595],
    ];

    #[test]
    fn derived_matrix_matches_shader_constants() {
        let derived = gamut_conversion_matrix(ColorPrimaries::Srgb, ColorPrimaries::Bt2020);
        for row in 0..3 {
            for col in 0..3 {
                let delta = (derived[row][col] - SHADER_BT709_TO_BT2020[row][col])
                    .abs();
                assert!(
                    delta < 2e-4,
                    "matrix[{row}][{col}]: derived {} vs shader {}",
                    derived[row][col],
                    SHADER_BT709_TO_BT2020[row][col]
                );
            }
        }
    }

    #[test]
    fn white_maps_to_white() {
        // Both gamuts share D65, so full white must stay full white.
        let m = gamut_conversion_matrix(
            ColorPrimaries::Srgb,
            ColorPrimaries::Bt2020
        );
        let white = mat3_mul_vec(
            &m,
            &[
                1.0,
                1.0,
                1.0
            ]
        );
        for channel in white {
            assert!((channel - 1.0)
                        .abs() < 1e-6, "white drifted: {white:?}");
        }
    }

    #[test]
    fn pq_reference_points() {
        // Published reference values for the ST 2084 curve.
        assert!((pq_inverse_eotf(0.0) - 0.0)
            .abs() < 1e-6);
        assert!((pq_inverse_eotf(10000.0) - 1.0)
            .abs() < 1e-6);
        // 100 cd/m² ≈ 0.508 PQ, 203 cd/m² (HDR reference white) ≈ 0.5806 PQ.
        assert!((pq_inverse_eotf(100.0) - 0.5081)
            .abs() < 1e-3);
        assert!((pq_inverse_eotf(203.0) - 0.5806)
            .abs() < 1e-3);
    }

    /// Pins the SDR display EOTF the HDR encode pass uses, because nothing
    /// else does and the wrong choice is invisible to every other test here.
    ///
    /// `output_encode.frag` originally decoded SDR with the IEC 61966-2-1
    /// piecewise curve. That function is the inverse of the sRGB *encoding*
    /// transfer, whose `c/12.92` toe exists for numerical reasons when
    /// encoding; real SDR displays follow a power law instead. Decoding with
    /// the piecewise curve emits ~60x the light of gamma 2.2 at code 1 and
    /// ~16x at code 3, and PQ then spends its densest codes on that error.
    ///
    /// Measured 9/9/2026 on a Philips 8505: every patch of a static test
    /// pattern except (0,0,0) glowed, and dark-scene compression artefacts
    /// read as bright green. KWin fixed the identical bug the identical way,
    /// and the wayland colour-management protocol keeps `srgb` and `gamma22`
    /// as separate transfer functions precisely because they are not the same
    /// curve.
    ///
    /// The bare power law that replaced it still ran to true black, which no
    /// SDR display does either: BT.1886 anchors code 0 at the source black
    /// level, and without that pedestal near-black step *ratios* survive into
    /// PQ at full strength — the dark-scene blocks at (2,1,10) and (0,1,16)
    /// sat 2.5x apart in luminance against 1.11x over a laptop floor, subtle
    /// there and plainly blocky on the TV (20/10/2026). The pedestal form
    /// below is 1.14x. That ratio, not any single code's landing spot, is
    /// what this test exists to hold.
    ///
    /// Note this pins the default for UNTAGGED surfaces. A client that binds
    /// `wp_color_manager_v1` and declares TRANSFER_FUNCTION_SRGB must still be
    /// decoded piecewise — that is a different question from what an untagged
    /// 8-bit desktop buffer means.
    #[test]
    fn sdr_eotf_is_bt1886_not_the_srgb_toe() {
        fn piecewise(c: f64) -> f64 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn to_pq10(encoded: f64, eotf: impl Fn(f64) -> f64) -> f64 {
            pq_inverse_eotf(eotf(encoded) * 203.0) * 1023.0
        }

        let gamma = crate::backend::hdr_pipeline::sdr_reference_gamma();
        assert!(
            (gamma - 2.2).abs() < 1e-6,
            "default SDR gamma changed: {gamma}"
        );
        // Pin the defaults the numbers below are computed against, so an
        // env override in the test environment fails loudly here instead of
        // producing a confusing mismatch further down. (They are OnceLock-
        // cached; the values are the compiled-in defaults when unset.)
        let black = crate::backend::hdr_pipeline::sdr_black_nits();
        assert!(
            (black - 0.1).abs() < 1e-6,
            "default SDR black changed: {black}"
        );
        let k = crate::backend::hdr_pipeline::sdr_black_k(black, 203.0, gamma);
        let bt1886 = |c: f64| crate::backend::hdr_pipeline::sdr_eotf(c as f32, gamma, k) as f64;

        // Code 0 is the authored SDR black itself: PQ(0.1 cd/m2) = 63.8.
        let code0 = to_pq10(0.0, bt1886);
        assert!(
            (code0 - 63.8).abs() < 1.0,
            "sRGB code 0 should land on the 0.1 cd/m2 pedestal at PQ10 ~63.8, got {code0}"
        );
        assert!(
            to_pq10(0.0, piecewise) < 0.01,
            "sanity: the rejected piecewise toe still runs to true black"
        );

        // Code 3 is where dark-scene artefacts live. It sits just above the
        // pedestal now (0.198 cd/m2), not at the bare power law's 23.6.
        let code3 = 3.0 / 255.0;
        let with_1886 = to_pq10(code3, bt1886);
        assert!(
            (with_1886 - 84.3).abs() < 1.0,
            "sRGB code 3 should reach PQ10 ~84.3 under the BT.1886 form, got {with_1886}"
        );

        // The point of the pedestal: neighbouring compression blocks in a
        // real dark frame stay close in luminance, as they do on the SDR
        // panel they were authored on. The bare power law puts them 2.5x
        // apart; over a 0.25 cd/m2 laptop floor they measure 1.11x.
        let lum = |rgb: [u8; 3], eotf: &dyn Fn(f64) -> f64| {
            [0.2126, 0.7152, 0.0722]
                .into_iter()
                .zip(rgb)
                .map(|(w, c)| w * eotf(f64::from(c) / 255.0))
                .sum::<f64>()
                * 203.0
        };
        let gamma64 = f64::from(gamma);
        let ratio_1886 = lum([0, 1, 16], &bt1886) / lum([2, 1, 10], &bt1886);
        let ratio_bare =
            lum([0, 1, 16], &|c: f64| c.powf(gamma64)) / lum([2, 1, 10], &|c: f64| c.powf(gamma64));
        assert!(
            ratio_bare > 2.4,
            "sanity: the bare power law still separates the blocks, got {ratio_bare}"
        );
        assert!(
            ratio_1886 < 1.25,
            "pedestal stopped compressing near-black steps: ratio {ratio_1886}"
        );

        // Anchored at white: out of the shadows the form must not wander
        // far from the curve it replaced (worst at mid-grey, ~8 PQ10 codes
        // from the piecewise decode, nil at 255).
        for code in [128.0, 200.0, 255.0] {
            let encoded = code / 255.0;
            let g = to_pq10(encoded, bt1886);
            let t = to_pq10(encoded, piecewise);
            assert!(
                (g - t).abs() < 9.0,
                "curves diverge at code {code}: BT.1886 {g} vs piecewise {t}"
            );
        }
        assert!((to_pq10(1.0, bt1886) - to_pq10(1.0, piecewise)).abs() < 0.5);
    }

    #[test]
    fn pq_roundtrip() {
        for nits in [0.005, 1.0, 80.0, 203.0, 1000.0, 4000.0, 10000.0] {
            let roundtrip = pq_eotf(
                pq_inverse_eotf(
                    nits
                )
            );
            assert!(
                (roundtrip - nits)
                    .abs() / nits < 1e-6,
                "PQ roundtrip drifted: {nits} -> {roundtrip}"
            );
        }
    }
}
