// Fills the whole output with the picture, as big as fits and centred, over a blurred, dimmed copy of it that covers the output.
// It needs --keepaspect=no, as hooks only draw inside the video's rectangle, and that makes the rectangle the whole output. The
// The placeholders are filled in by rsimgv, as --glsl-shader-opts is not available to the renderer libmpv embeds.

//!DESC rsimgv backdrop: shrink the part that covers the output
//!HOOK MAIN
//!BIND HOOKED
//!SAVE RSIMGV_SHRUNK
//!WIDTH OUTPUT.w 8 /
//!HEIGHT OUTPUT.h 8 /
vec4 hook() {
    float scale = max(target_size.x / input_size.x, target_size.y / input_size.y) * @ZOOM@;
    vec2 shown = input_size * scale;
    vec2 uv = (HOOKED_pos * target_size - (target_size - shown) / 2.0) / shown;
    vec2 step_size = 8.0 / shown / 4.0;
    vec3 sum = vec3(0.0);
    for (int y = 0; y < 4; y++) {
        for (int x = 0; x < 4; x++) {
            sum += HOOKED_tex(uv + (vec2(float(x), float(y)) - 1.5) * step_size).rgb;
        }
    }
    return vec4(sum / 16.0, 1.0);
}

//!DESC rsimgv backdrop: blur across
//!HOOK MAIN
//!BIND RSIMGV_SHRUNK
//!SAVE RSIMGV_ACROSS
//!WIDTH RSIMGV_SHRUNK.w
//!HEIGHT RSIMGV_SHRUNK.h
vec4 hook() {
    vec3 sum = vec3(0.0);
    float total = 0.0;
    for (int i = -@RADIUS@; i <= @RADIUS@; i++) {
        float weight = exp(-0.5 * float(i * i) / (@SIGMA@ * @SIGMA@));
        sum += RSIMGV_SHRUNK_tex(RSIMGV_SHRUNK_pos + vec2(float(i) * RSIMGV_SHRUNK_pt.x, 0.0)).rgb * weight;
        total += weight;
    }
    return vec4(sum / total, 1.0);
}

//!DESC rsimgv backdrop: blur down
//!HOOK MAIN
//!BIND RSIMGV_ACROSS
//!SAVE RSIMGV_BLURRED
//!WIDTH RSIMGV_ACROSS.w
//!HEIGHT RSIMGV_ACROSS.h
vec4 hook() {
    vec3 sum = vec3(0.0);
    float total = 0.0;
    for (int i = -@RADIUS@; i <= @RADIUS@; i++) {
        float weight = exp(-0.5 * float(i * i) / (@SIGMA@ * @SIGMA@));
        sum += RSIMGV_ACROSS_tex(RSIMGV_ACROSS_pos + vec2(0.0, float(i) * RSIMGV_ACROSS_pt.y)).rgb * weight;
        total += weight;
    }
    return vec4(sum / total, 1.0);
}

//!DESC rsimgv backdrop: the picture over it
//!HOOK SCALED
//!BIND HOOKED
//!BIND RSIMGV_BLURRED
vec4 hook() {
    float scale = min(target_size.x / input_size.x, target_size.y / input_size.y);
    vec2 shown = input_size * scale;
    vec2 uv = (HOOKED_pos * target_size - (target_size - shown) / 2.0) / shown;
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return vec4(RSIMGV_BLURRED_tex(HOOKED_pos).rgb * @DIM@, 1.0);
    }
    // The scaler stretched the picture over the whole output, so it is squeezed back, averaging what each pixel covers.
    vec2 squeeze = target_size / shown;
    int taps = int(clamp(ceil(max(squeeze.x, squeeze.y)), 1.0, 8.0));
    vec2 along = squeeze.x > squeeze.y ? vec2(1.0 / shown.x, 0.0) : vec2(0.0, 1.0 / shown.y);
    vec3 sum = vec3(0.0);
    for (int i = 0; i < taps; i++) {
        sum += HOOKED_tex(uv + along * ((float(i) + 0.5) / float(taps) - 0.5)).rgb;
    }
    return vec4(sum / float(taps), 1.0);
}
