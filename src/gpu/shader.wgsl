struct Uniforms {
    viewport_width: f32,
    viewport_height: f32,
    cell_width: f32,
    cell_height: f32,
    atlas_width: f32,
    atlas_height: f32,
    render_phase: f32,
    scroll_pixel_offset: f32,
    fixed_start_row: f32,
    fixed_pixel_offset: f32,
    viewport_origin: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(0) @binding(3) var color_atlas_texture: texture_2d<f32>;
@group(0) @binding(4) var color_atlas_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) glyph_uv0: vec2<f32>,
    @location(1) @interpolate(flat) glyph_uv1: vec2<f32>,
    @location(2) fg_color: vec4<f32>,
    @location(3) bg_color: vec4<f32>,
    @location(4) @interpolate(flat) flags: u32,
    @location(5) @interpolate(flat) cell_rect: vec4<f32>,
    @location(6) @interpolate(flat) glyph_offset: vec2<f32>,
    @location(7) cell_px_pos: vec2<f32>,
    @location(9) @interpolate(flat) fixed_row: u32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    // Per-instance attributes
    @location(0) col_row: vec2<u32>,
    @location(1) glyph_uv0: vec2<f32>,
    @location(2) glyph_uv1: vec2<f32>,
    @location(3) fg_color: vec4<f32>,
    @location(4) bg_color: vec4<f32>,
    @location(5) flags: u32,
    @location(6) glyph_offset: vec2<f32>,
) -> VertexOutput {
    // Generate quad vertices: two triangles
    var quad_x = array<f32, 6>(0.0, 1.0, 0.0, 0.0, 1.0, 1.0);
    var quad_y = array<f32, 6>(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);

    let qx = quad_x[vertex_index];
    let qy = quad_y[vertex_index];

    let is_wide = (flags & 2u) != 0u;
    var cell_w = u.cell_width;
    if is_wide {
        cell_w = u.cell_width * 2.0;
    }

    let has_glyph = (flags & 1u) != 0u;
    let glyph_size = (glyph_uv1 - glyph_uv0) * vec2<f32>(u.atlas_width, u.atlas_height);
    let foreground_pass = u.render_phase >= 0.5;

    let grid_origin = vec2<f32>(f32(col_row.x) * u.cell_width, f32(col_row.y) * u.cell_height);
    let fixed = u.fixed_start_row >= 0.0 && f32(col_row.y) >= u.fixed_start_row;
    let offset = select(u.scroll_pixel_offset, u.fixed_pixel_offset, fixed);
    var cell_origin = grid_origin;
    var cell_size = vec2<f32>(cell_w, u.cell_height);

    // Snap shared cell edges, independently of ink bounds or has_glyph. This
    // keeps the baseline common and decorations contiguous across spaces and
    // ligature-covered cells. Rounding each glyph's origin instead changes
    // half-pixel tie decisions with its bearing. Backgrounds retain the
    // continuous grid; scrolling translates the entire foreground together.
    if foreground_pass {
        cell_origin = round(grid_origin);
        // Derive the far edge from its grid index too, so adjacent cells use
        // exactly the same float before rounding, even at half-pixel ties.
        let end_col = col_row.x + select(1u, 2u, is_wide);
        let grid_end = vec2<f32>(f32(end_col) * u.cell_width, f32(col_row.y + 1u) * u.cell_height);
        cell_size = round(grid_end) - cell_origin;
    }
    cell_origin.y -= offset;

    var quad_min = vec2<f32>(0.0, 0.0);
    var quad_max = cell_size;
    if foreground_pass && has_glyph {
        quad_min = min(quad_min, glyph_offset);
        quad_max = max(quad_max, glyph_offset + glyph_size);
    }

    let px_in_cell = vec2<f32>(
        mix(quad_min.x, quad_max.x, qx),
        mix(quad_min.y, quad_max.y, qy),
    );

    // Cell position relative to the viewport, with continuous smooth scrolling.
    let px = cell_origin.x + px_in_cell.x;
    let py = cell_origin.y + px_in_cell.y;

    // Convert to NDC (viewport is set to content_rect by egui-wgpu)
    let ndc_x = (px / u.viewport_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (py / u.viewport_height) * 2.0;

    var out: VertexOutput;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.glyph_uv0 = glyph_uv0;
    out.glyph_uv1 = glyph_uv1;
    out.fg_color = fg_color;
    out.bg_color = bg_color;
    out.flags = flags;
    out.cell_rect = vec4<f32>(u.viewport_origin + cell_origin, cell_size);
    out.glyph_offset = glyph_offset;
    out.cell_px_pos = px_in_cell;
    out.fixed_row = select(0u, 1u, fixed);

    return out;
}

// Gamma ↔ Linear conversion functions (matching egui exactly)
fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

fn gamma_from_linear_rgb(linear: vec3<f32>) -> vec3<f32> {
    let cutoff = linear <= vec3<f32>(0.0031308);
    let lower = linear * vec3<f32>(12.92);
    let higher = vec3<f32>(1.055) * pow(linear, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(higher, lower, cutoff);
}

fn perceived_luminance(lin: vec3<f32>) -> f32 {
    return dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Coverage correction for linear-space glyph compositing.
//
// Pure linear blending is physically correct but thins bright-on-dark text,
// because font stroke weights were tuned against perceptual (gamma-space)
// rasterizers. Pure gamma-space blending keeps the expected weight but shifts
// hues on colored text. This picks per-channel blend factors so the blended
// *luminance* matches gamma-space blending while chroma still mixes linearly —
// the same idea as Ghostty's "linear-corrected" text blending.
fn corrected_coverage(cov: vec3<f32>, fg_lin: vec3<f32>, bg_lin: vec3<f32>) -> vec3<f32> {
    let lum_fg = perceived_luminance(fg_lin);
    let lum_bg = perceived_luminance(bg_lin);
    let delta = lum_fg - lum_bg;
    if abs(delta) < 1e-4 {
        return cov;
    }
    let gamma_fg = gamma_from_linear_rgb(vec3<f32>(lum_fg)).x;
    let gamma_bg = gamma_from_linear_rgb(vec3<f32>(lum_bg)).x;
    let target_lum = linear_from_gamma_rgb(mix(vec3<f32>(gamma_bg), vec3<f32>(gamma_fg), cov));
    return clamp((target_lum - vec3<f32>(lum_bg)) / delta, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Derive decorations from framebuffer pixel centers and a flat cell origin.
// Interpolating normalized coordinates across glyph-sized quads introduces
// different errors for ascenders/descenders exactly at a line boundary: a
// straight underline can otherwise jump one pixel between adjacent glyphs.
// Both foreground blend paths use the same cell geometry and style rules.
fn draws_decoration(in: VertexOutput) -> bool {
    let local = (in.position.xy - in.cell_rect.xy) / in.cell_rect.zw;
    if any(local < vec2<f32>(0.0)) || any(local >= vec2<f32>(1.0)) {
        return false;
    }

    let underline_style = (in.flags >> 2u) & 7u;
    let band = 1.0 - local.y;
    var underline = false;
    if underline_style == 1u {
        underline = band < 0.08;
    } else if underline_style == 2u {
        underline = (band > 0.02 && band < 0.06) || (band > 0.09 && band < 0.13);
    } else if underline_style == 3u {
        let wave_y = 0.92 + sin(local.x * 6.283185) * 0.03;
        underline = abs(local.y - wave_y) < 0.025;
    } else if underline_style == 4u {
        underline = band < 0.08 && fract(local.x * 4.0) < 0.5;
    } else if underline_style == 5u {
        underline = band < 0.08 && fract(local.x * 2.0) < 0.6;
    }
    let strikethrough = (in.flags & 32u) != 0u && abs(local.y - 0.5) < 0.04;
    return underline || strikethrough;
}

// Compute final fragment color (shared by both entry points).
// `linear_target` marks the sRGB-framebuffer entry point, whose translucent
// output is alpha-blended by the hardware in linear space.
fn compute_fragment_color(in: VertexOutput, linear_target: bool) -> vec4<f32> {
    let has_glyph = (in.flags & 1u) != 0u;
    let is_color_glyph = (in.flags & 64u) != 0u;
    let background_pass = u.render_phase < 0.5;

    // Background pass draws only cell fills. Foreground pass starts transparent.
    var color = select(vec4<f32>(0.0, 0.0, 0.0, 0.0), in.bg_color, background_pass);

    if background_pass {
        return color;
    }

    // Composite glyph foreground
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
                color = vec4<f32>(texel.rgb, a);
            }
        } else {
            let texel = textureSample(atlas_texture, atlas_sampler, uv);
            // Atlas texels hold raw linear coverage (per RGB subpixel channel
            // when LCD rendering is enabled, replicated gray otherwise).
            let cov = texel.rgb * in_bounds;
            let a = dot(cov, vec3<f32>(1.0 / 3.0));
            if a > 0.001 {
                let fg_lin = linear_from_gamma_rgb(in.fg_color.rgb);
                let bg_lin = linear_from_gamma_rgb(in.bg_color.rgb);
                if in.bg_color.a <= 0.001 {
                    // Default cells are transparent over the Kitty image
                    // layer, so alpha-blend the glyph instead of baking the
                    // terminal base color into its antialiased edge. The cell's
                    // bg rgb still carries the terminal base color, which is
                    // the backdrop in all but the rare image-covered cells —
                    // use it to weight-correct the hardware linear blend.
                    var alpha = a;
                    if linear_target {
                        alpha = corrected_coverage(vec3<f32>(a), fg_lin, bg_lin).x;
                    }
                    color = vec4<f32>(in.fg_color.rgb, alpha);
                } else {
                    let ac = corrected_coverage(cov, fg_lin, bg_lin);
                    let blended_lin = mix(bg_lin, fg_lin, ac);
                    color = vec4<f32>(gamma_from_linear_rgb(blended_lin), 1.0);
                }
            }
        }
    }

    if draws_decoration(in) {
        color = vec4<f32>(in.fg_color.rgb, 1.0);
    }

    return color;
}

@fragment
fn fs_main_gamma(in: VertexOutput) -> @location(0) vec4<f32> {
    canvas_clip(in);
    return compute_fragment_color(in, false);
}

@fragment
fn fs_main_linear(in: VertexOutput) -> @location(0) vec4<f32> {
    canvas_clip(in);
    let result_gamma = compute_fragment_color(in, true);
    let result_linear_rgb = linear_from_gamma_rgb(result_gamma.rgb);
    return vec4<f32>(result_linear_rgb, result_gamma.a);
}

fn canvas_clip(in: VertexOutput) {
    if u.fixed_start_row >= 0.0 {
        let dock_top = u.fixed_start_row * u.cell_height - u.fixed_pixel_offset;
        let canvas_y = in.position.y - u.viewport_origin.y;
        if (in.fixed_row == 0u && canvas_y >= dock_top) || (in.fixed_row == 1u && canvas_y < dock_top) { discard; }
    }
}
