struct Uniforms {
    viewport_width: f32,
    viewport_height: f32,
    cell_width: f32,
    cell_height: f32,
    atlas_width: f32,
    atlas_height: f32,
    render_phase: f32,
    scroll_pixel_offset: f32,
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
    @location(5) cell_local_pos: vec2<f32>,
    @location(6) @interpolate(flat) glyph_offset: vec2<f32>,
    @location(7) cell_px_pos: vec2<f32>,
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

    var quad_min = vec2<f32>(0.0, 0.0);
    var quad_max = vec2<f32>(cell_w, u.cell_height);
    if foreground_pass && has_glyph {
        quad_min = min(quad_min, glyph_offset);
        quad_max = max(quad_max, glyph_offset + glyph_size);
    }

    let px_in_cell = vec2<f32>(
        mix(quad_min.x, quad_max.x, qx),
        mix(quad_min.y, quad_max.y, qy),
    );

    // Snap the foreground glyph quad to the integer physical-pixel grid so the
    // 1:1 atlas→screen sampling stays crisp (fractional cell origins otherwise get
    // softened by the linear sampler). Snap is derived from the grid position only,
    // excluding the smooth-scroll offset, so scrolling still translates continuously.
    // Background fills are left unsnapped to keep cells tiling seamlessly.
    var snap = vec2<f32>(0.0, 0.0);
    if foreground_pass && has_glyph {
        let origin_x = f32(col_row.x) * u.cell_width + glyph_offset.x;
        let origin_y = f32(col_row.y) * u.cell_height + glyph_offset.y;
        snap = vec2<f32>(round(origin_x) - origin_x, round(origin_y) - origin_y);
    }

    // Cell position in physical pixels relative to viewport origin (with smooth scroll offset)
    let px = f32(col_row.x) * u.cell_width + px_in_cell.x + snap.x;
    let py = f32(col_row.y) * u.cell_height + px_in_cell.y - u.scroll_pixel_offset + snap.y;

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
    out.cell_local_pos = px_in_cell / vec2<f32>(cell_w, u.cell_height);
    out.glyph_offset = glyph_offset;
    out.cell_px_pos = px_in_cell;

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

// Compute final fragment color (shared by both entry points).
// `linear_target` marks the sRGB-framebuffer entry point, whose translucent
// output is alpha-blended by the hardware in linear space.
fn compute_fragment_color(in: VertexOutput, linear_target: bool) -> vec4<f32> {
    let has_glyph = (in.flags & 1u) != 0u;
    let underline_style = (in.flags >> 2u) & 7u; // bits 2-4: 0=none,1=single,2=double,3=curly,4=dotted,5=dashed
    let has_strikethrough = (in.flags & 32u) != 0u;
    let is_color_glyph = (in.flags & 64u) != 0u;
    let background_pass = u.render_phase < 0.5;

    let is_wide = (in.flags & 2u) != 0u;
    var cell_w = u.cell_width;
    if is_wide {
        cell_w = u.cell_width * 2.0;
    }

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

    // Underline styles
    let within_cell = in.cell_local_pos.x >= 0.0 && in.cell_local_pos.x <= 1.0
        && in.cell_local_pos.y >= 0.0 && in.cell_local_pos.y <= 1.0;

    if underline_style > 0u && within_cell {
        let y_pos = in.cell_local_pos.y;
        let x_pos = in.cell_local_pos.x;
        var draw_underline = false;

        if underline_style == 1u {
            // Single: 1px line near bottom
            draw_underline = (1.0 - y_pos) < 0.08;
        } else if underline_style == 2u {
            // Double: two lines
            let band = 1.0 - y_pos;
            draw_underline = (band > 0.02 && band < 0.06) || (band > 0.09 && band < 0.13);
        } else if underline_style == 3u {
            // Curly: sine wave at bottom
            let wave_y = 0.92 + sin(x_pos * 6.283185) * 0.03;
            draw_underline = abs(y_pos - wave_y) < 0.025;
        } else if underline_style == 4u {
            // Dotted: evenly spaced dots
            let dot_phase = fract(x_pos * 4.0);
            draw_underline = (1.0 - y_pos) < 0.08 && dot_phase < 0.5;
        } else if underline_style == 5u {
            // Dashed: longer segments
            let dash_phase = fract(x_pos * 2.0);
            draw_underline = (1.0 - y_pos) < 0.08 && dash_phase < 0.6;
        }

        if draw_underline {
            color = vec4<f32>(in.fg_color.rgb, 1.0);
        }
    }

    // Strikethrough: 1-2px line at middle of cell
    if has_strikethrough && within_cell {
        let mid = abs(in.cell_local_pos.y - 0.5);
        if mid < 0.04 {
            color = vec4<f32>(in.fg_color.rgb, 1.0);
        }
    }

    return color;
}

@fragment
fn fs_main_gamma(in: VertexOutput) -> @location(0) vec4<f32> {
    return compute_fragment_color(in, false);
}

@fragment
fn fs_main_linear(in: VertexOutput) -> @location(0) vec4<f32> {
    let result_gamma = compute_fragment_color(in, true);
    let result_linear_rgb = linear_from_gamma_rgb(result_gamma.rgb);
    return vec4<f32>(result_linear_rgb, result_gamma.a);
}
