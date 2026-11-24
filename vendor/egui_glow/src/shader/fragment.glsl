#ifdef GL_ES
    precision mediump float;
#endif

uniform sampler2D u_sampler;
uniform bool u_font_preblend;
uniform bool u_font_texture;
uniform bool u_subpixel_text;

#if NEW_SHADER_INTERFACE
    in vec4 v_rgba_in_gamma;
    in vec2 v_tc;
    #if DUAL_SOURCE
        layout(location=0,index=0) out vec4 f_color;
        layout(location=0,index=1) out vec4 f_coverage;
    #else
        out vec4 f_color;
    #endif
    // a dirty hack applied to support webGL2
    #define gl_FragColor f_color
    #define texture2D texture
#else
    varying vec4 v_rgba_in_gamma;
    varying vec2 v_tc;
#endif

// -----------------------------------------------
// Adapted from
// https://www.shadertoy.com/view/llVGzG
// Originally presented in:
// Jimenez 2014, "Next Generation Post-Processing in Call of Duty"
//
// A good overview can be found in
// https://blog.demofox.org/2022/01/01/interleaved-gradient-noise-a-different-kind-of-low-discrepancy-sequence/
// via https://github.com/rerun-io/rerun/
float interleaved_gradient_noise(vec2 n) {
    float f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

vec3 dither_interleaved(vec3 rgb, float levels) {
    float noise = interleaved_gradient_noise(gl_FragCoord.xy);
    // scale down the noise slightly to ensure flat colors aren't getting dithered
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

// 0-1 sRGB gamma  from  0-1 linear
vec3 srgb_gamma_from_linear(vec3 rgb) {
    bvec3 cutoff = lessThan(rgb, vec3(0.0031308));
    vec3 lower = rgb * vec3(12.92);
    vec3 higher = vec3(1.055) * pow(rgb, vec3(1.0 / 2.4)) - vec3(0.055);
    return mix(higher, lower, vec3(cutoff));
}

// 0-1 sRGBA gamma  from  0-1 linear
vec4 srgba_gamma_from_linear(vec4 rgba) {
    return vec4(srgb_gamma_from_linear(rgba.rgb), rgba.a);
}

float corrected_coverage(float a,float src) {
    if(a<=0.000001||a>=0.999999)return a;
    float dst=1.0-src;
    float lin_dst=pow(dst,1.4);
    a+=(1.0-a)*lin_dst*a;
    if(abs(src-dst)>=1.0/256.0){
        float out_color=pow(pow(src,1.4)*a+(1.0-a)*lin_dst,1.0/1.4);
        a=clamp((out_color-dst)/(src-dst),0.0,1.0);
    }
    return a;
}

void main() {
#if SRGB_TEXTURES
    vec4 sampled=texture2D(u_sampler,v_tc);
    vec4 texture_in_gamma = u_font_texture ? sampled : srgba_gamma_from_linear(sampled);
#else
    vec4 texture_in_gamma = texture2D(u_sampler, v_tc);
#endif

    // Native masks contain coverage, not egui's legacy weight adjustment.
    // Correct for the foreground luminance before gamma-space source-over.
    // The perceptual inverse estimates the destination, as in Skia mask preblend.
    if (u_font_preblend) {
        vec3 foreground=v_rgba_in_gamma.a>0.0?clamp(v_rgba_in_gamma.rgb/v_rgba_in_gamma.a,0.0,1.0):vec3(0.0);
        #if DUAL_SOURCE
        if(u_subpixel_text){
            vec3 mask=vec3(corrected_coverage(texture_in_gamma.r,foreground.r),
                corrected_coverage(texture_in_gamma.g,foreground.g),corrected_coverage(texture_in_gamma.b,foreground.b));
            float alpha=max(mask.r,max(mask.g,mask.b))*v_rgba_in_gamma.a;
            f_color=vec4(v_rgba_in_gamma.rgb*mask,alpha);
            f_coverage=vec4(mask*v_rgba_in_gamma.a,alpha);
            return;
        }
        #endif
        texture_in_gamma=vec4(corrected_coverage(texture_in_gamma.a,foreground.g));
    }

    // We multiply the colors in gamma space, because that's the only way to get text to look right.
    vec4 frag_color_gamma = v_rgba_in_gamma * texture_in_gamma;

    // Dither the float color down to eight bits to reduce banding.
    // This step is optional for egui backends.
#if DITHERING
    frag_color_gamma.rgb = dither_interleaved(frag_color_gamma.rgb, 256.);
#endif
    gl_FragColor = frag_color_gamma;
    #if DUAL_SOURCE
    f_coverage=vec4(frag_color_gamma.a);
    #endif
}
