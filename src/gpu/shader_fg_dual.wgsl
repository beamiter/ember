// Foreground-pass fragment entry points using dual-source blending.
//
// This file is appended to shader.wgsl (with `enable dual_source_blending;`
// prepended) when the device offers DUAL_SOURCE_BLENDING, and the resulting
// module drives the foreground pipeline only. The blend state is
//   final = src.color + dst * (1 - src.mask)     (per channel)
// which lets translucent glyphs carry a *per-channel* (LCD subpixel) alpha
// over the real destination — single-source alpha blending can only apply one
// scalar alpha, collapsing subpixel coverage to grayscale on default-background
// cells.

struct DualSourceOutput {
    @location(0) @blend_src(0) color: vec4<f32>,
    @location(0) @blend_src(1) mask: vec4<f32>,
}

// Foreground-pass fragment logic with per-channel output alpha.
// Tracks an unpremultiplied gamma-space rgb plus a per-channel mask, and
// premultiplies in the target space at the end (premultiplying before the
// gamma→linear conversion would distort the color).
fn compute_fragment_dual(in: VertexOutput, linear_target: bool) -> DualSourceOutput {
    let has_glyph = (in.flags & 1u) != 0u;
    let is_color_glyph = (in.flags & 64u) != 0u;

    var rgb = vec3<f32>(0.0);
    var mask = vec3<f32>(0.0);

    if has_glyph {
        let glyph_size = (in.glyph_uv1 - in.glyph_uv0) * vec2<f32>(u.atlas_width, u.atlas_height);

        let rel = in.cell_px_pos - in.glyph_offset;
        let t = clamp(rel / max(glyph_size, vec2<f32>(1.0, 1.0)), vec2<f32>(0.0), vec2<f32>(1.0));
        let uv = in.glyph_uv0 + t * (in.glyph_uv1 - in.glyph_uv0);

        let in_bounds = step(0.0, rel.x) * step(0.0, rel.y)
                      * step(rel.x, glyph_size.x) * step(rel.y, glyph_size.y);

        if is_color_glyph {
            let texel = textureSample(color_atlas_texture, color_atlas_sampler, uv);
            let a = texel.a * in_bounds;
            if a > 0.001 {
                rgb = texel.rgb;
                mask = vec3<f32>(a);
            }
        } else {
            let texel = textureSample(atlas_texture, atlas_sampler, uv);
            let cov = texel.rgb * in_bounds;
            let a = dot(cov, vec3<f32>(1.0 / 3.0));
            if a > 0.001 {
                let fg_lin = linear_from_gamma_rgb(in.fg_color.rgb);
                let bg_lin = linear_from_gamma_rgb(in.bg_color.rgb);
                if in.bg_color.a <= 0.001 {
                    // Transparent (default-background) cell: blend against the
                    // real destination with per-channel coverage. For a linear
                    // target the hardware blend happens in linear space, so
                    // weight-correct the coverage against the carried terminal
                    // base color; a gamma target blends perceptually already.
                    rgb = in.fg_color.rgb;
                    if linear_target {
                        mask = corrected_coverage(cov, fg_lin, bg_lin);
                    } else {
                        mask = cov;
                    }
                } else {
                    // Opaque cell background: resolve the exact blend here and
                    // overwrite the destination.
                    let ac = corrected_coverage(cov, fg_lin, bg_lin);
                    rgb = gamma_from_linear_rgb(mix(bg_lin, fg_lin, ac));
                    mask = vec3<f32>(1.0);
                }
            }
        }
    }

    // Underline styles (same shapes as the single-source path)
    if draws_decoration(in) {
        rgb = in.fg_color.rgb;
        mask = vec3<f32>(1.0);
    }

    var rgb_target = rgb;
    if linear_target {
        rgb_target = linear_from_gamma_rgb(rgb);
    }
    let mask_a = dot(mask, vec3<f32>(1.0 / 3.0));

    var out: DualSourceOutput;
    out.color = vec4<f32>(rgb_target * mask, mask_a);
    out.mask = vec4<f32>(mask, mask_a);
    return out;
}

@fragment
fn fs_fg_dual_gamma(in: VertexOutput) -> DualSourceOutput {
    canvas_clip(in);
    return compute_fragment_dual(in, false);
}

@fragment
fn fs_fg_dual_linear(in: VertexOutput) -> DualSourceOutput {
    canvas_clip(in);
    return compute_fragment_dual(in, true);
}
