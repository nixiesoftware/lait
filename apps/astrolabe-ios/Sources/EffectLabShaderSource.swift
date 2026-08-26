enum EffectLabShaderSource {
    static let code = #"""
    #include <metal_stdlib>
    using namespace metal;

    struct EffectUniforms {
        float4 geometry;
        float4 shape;
        float4 motion;
        float4 finishing;
        float4 composite;
    };

    struct RasterData {
        float4 position [[position]];
    };

    vertex RasterData effectLabVertex(uint vertexID [[vertex_id]]) {
        const float2 positions[3] = {
            float2(-1.0, -1.0),
            float2( 3.0, -1.0),
            float2(-1.0,  3.0)
        };
        RasterData out;
        out.position = float4(positions[vertexID], 0.0, 1.0);
        return out;
    }

    static float hash21(float2 p) {
        p = fract(p * float2(123.34, 456.21));
        p += dot(p, p + 45.32);
        return fract(p.x * p.y);
    }

    static float valueNoise(float2 p) {
        float2 cell = floor(p);
        float2 local = fract(p);
        float2 curve = local * local * (3.0 - 2.0 * local);
        float a = hash21(cell);
        float b = hash21(cell + float2(1.0, 0.0));
        float c = hash21(cell + float2(0.0, 1.0));
        float d = hash21(cell + float2(1.0, 1.0));
        return mix(mix(a, b, curve.x), mix(c, d, curve.x), curve.y);
    }

    static float fbm(float2 p, float detail) {
        float value = 0.0;
        float amplitude = 0.55;
        float normalization = 0.0;
        for (uint octave = 0; octave < 4; ++octave) {
            value += amplitude * valueNoise(p);
            normalization += amplitude;
            p = float2(1.72 * p.x - 1.18 * p.y, 1.18 * p.x + 1.72 * p.y) + 7.1;
            amplitude *= detail;
        }
        return value / max(normalization, 0.001);
    }

    static float3 spectrumRamp(float value, int scheme) {
        value = clamp(value, 0.0, 1.0);
        float3 c0;
        float3 c1;
        float3 c2;
        float3 c3;
        float3 c4;
        float3 c5;
        switch (scheme) {
            case 1:
                // Visible spectrum.
                c0 = float3(1.00, 0.00, 0.00);
                c1 = float3(1.00, 0.52, 0.00);
                c2 = float3(0.82, 1.00, 0.00);
                c3 = float3(0.00, 1.00, 0.55);
                c4 = float3(0.00, 0.34, 1.00);
                c5 = float3(0.58, 0.00, 1.00);
                break;
            case 2:
                // Aurora: terrestrial green into charged violet.
                c0 = float3(0.01, 0.05, 0.16);
                c1 = float3(0.00, 0.46, 0.32);
                c2 = float3(0.16, 1.00, 0.66);
                c3 = float3(0.00, 0.82, 1.00);
                c4 = float3(0.35, 0.28, 1.00);
                c5 = float3(0.82, 0.18, 1.00);
                break;
            case 3:
                // Bioluminescence: deep water into an excited bloom.
                c0 = float3(0.00, 0.04, 0.07);
                c1 = float3(0.00, 0.28, 0.34);
                c2 = float3(0.00, 0.80, 0.64);
                c3 = float3(0.54, 1.00, 0.22);
                c4 = float3(0.94, 1.00, 0.72);
                c5 = float3(1.00, 1.00, 1.00);
                break;
            case 4:
                // Plasma: ion blue through white-hot into magenta.
                c0 = float3(0.02, 0.00, 0.14);
                c1 = float3(0.05, 0.14, 0.88);
                c2 = float3(0.00, 0.82, 1.00);
                c3 = float3(1.00, 1.00, 1.00);
                c4 = float3(0.80, 0.18, 1.00);
                c5 = float3(1.00, 0.12, 0.56);
                break;
            case 5:
                // Solar: ember, corona, then blue-white flare.
                c0 = float3(0.10, 0.01, 0.00);
                c1 = float3(0.66, 0.03, 0.00);
                c2 = float3(1.00, 0.30, 0.00);
                c3 = float3(1.00, 0.92, 0.12);
                c4 = float3(1.00, 0.98, 0.78);
                c5 = float3(0.62, 0.84, 1.00);
                break;
            default:
                // Thermal: conventional infrared heat into UV reactivity.
                c0 = float3(0.10, 0.00, 0.012);
                c1 = float3(0.85, 0.01, 0.005);
                c2 = float3(1.00, 0.30, 0.015);
                c3 = float3(1.00, 0.88, 0.55);
                c4 = float3(0.10, 0.65, 1.00);
                c5 = float3(0.75, 0.02, 1.00);
                break;
        }

        float scaled = value * 5.0;
        int segment = min(int(floor(scaled)), 4);
        float t = smoothstep(0.0, 1.0, scaled - float(segment));
        switch (segment) {
            case 0: return mix(c0, c1, t);
            case 1: return mix(c1, c2, t);
            case 2: return mix(c2, c3, t);
            case 3: return mix(c3, c4, t);
            default: return mix(c4, c5, t);
        }
    }

    static float bayer4(float2 position) {
        const float matrix[16] = {
             0.0,  8.0,  2.0, 10.0,
            12.0,  4.0, 14.0,  6.0,
             3.0, 11.0,  1.0,  9.0,
            15.0,  7.0, 13.0,  5.0
        };
        int2 p = int2(position) & 3;
        return (matrix[p.y * 4 + p.x] + 0.5) / 16.0;
    }

    static float interleavedGradientNoise(float2 position) {
        // Jimenez-style interleaved gradient noise: deterministic, evenly
        // distributed in local neighborhoods, and cheaper than a noise texture.
        return fract(52.9829189 * fract(
            0.06711056 * position.x + 0.00583715 * position.y
        ));
    }

    static float3 ditheredQuantization(float3 color, float threshold, float ratio) {
        // Ratio has one legible meaning: crossfade between the original color
        // and a fixed seven-level threshold-quantized result.
        const float levels = 7.0;
        float3 quantized = floor(color * levels + threshold) / levels;
        return mix(color, quantized, ratio);
    }

    static float3 applyPost(
        float3 color,
        int effect,
        float amount,
        float2 uv,
        float2 position,
        float time
    ) {
        if (effect == 1) {
            float levels = mix(12.0, 3.0, amount);
            float3 posterized = floor(color * levels + 0.5) / levels;
            color = mix(color, posterized, amount);
        } else if (effect == 2) {
            float line = 0.5 + 0.5 * sin(position.y * 3.14159265);
            float mask = mix(1.0, mix(0.62, 1.04, line), amount);
            float3 phosphor = float3(
                0.94 + 0.06 * sin(position.x * 2.094),
                0.94 + 0.06 * sin(position.x * 2.094 + 2.094),
                0.94 + 0.06 * sin(position.x * 2.094 + 4.188)
            );
            color *= mask * mix(float3(1.0), phosphor, amount * 0.7);
        } else if (effect == 3) {
            float angle = atan2(uv.y, uv.x) + time * 0.08;
            float spread = amount * (0.08 + length(uv) * 0.14);
            float3 prism = color;
            prism.r *= 1.0 + sin(angle) * spread;
            prism.g *= 1.0 + sin(angle + 2.094) * spread;
            prism.b *= 1.0 + sin(angle + 4.188) * spread;
            prism += float3(0.07, 0.02, 0.09) * spread;
            color = mix(color, prism, amount);
        } else if (effect == 4) {
            float luminance = dot(color, float3(0.2126, 0.7152, 0.0722));
            float3 bleached = mix(float3(luminance), color, 0.28);
            bleached = smoothstep(float3(0.03), float3(0.92), bleached);
            bleached = mix(bleached, float3(1.0) - (float3(1.0) - color) * (float3(1.0) - bleached), 0.45);
            color = mix(color, bleached, amount);
        }
        return color;
    }

    fragment half4 effectLabFragment(
        RasterData in [[stage_in]],
        constant EffectUniforms &u [[buffer(0)]]
    ) {
        float2 resolution = u.geometry.xy;
        float time = u.geometry.z;
        float seed = u.geometry.w;
        float structure = u.shape.x;
        float detail = u.shape.y;
        float turbulence = u.shape.z;
        float softness = u.shape.w;
        float flow = u.motion.x;
        float glow = u.motion.y;
        float energy = u.motion.z;
        int packedStyle = int(round(u.motion.w));
        int family = packedStyle % 10;
        int spectrumIndex = packedStyle / 10;
        int ditherMode = int(round(u.finishing.x));
        float ditherRatio = u.finishing.y;
        int postEffect = int(round(u.finishing.z));
        float postAmount = u.finishing.w;

        float2 uv = (in.position.xy * 2.0 - resolution) / min(resolution.x, resolution.y);
        float2 drift = float2(time * flow, -time * flow * 0.63);
        float seedOffset = seed * 0.071;
        float intensity = 0.0;

        if (family == 0) {
            float2 p = uv * structure + drift;
            float2 warp = float2(
                fbm(p * 0.72 + seedOffset, detail),
                fbm(p * 0.72 + float2(8.3, -4.7) - seedOffset, detail)
            ) - 0.5;
            float density = fbm(p + warp * turbulence * 2.1, detail);
            float threshold = 0.62 - softness * 0.36 - energy * 0.08;
            intensity = smoothstep(threshold, threshold + 0.28, density);
            intensity += glow * 0.22 * smoothstep(0.35, 0.72, density);
        } else if (family == 1) {
            // A continuous height field lit from the side: broad folds carry
            // the silhouette, while a second frequency supplies woven sheen.
            float2 p = uv * structure;
            float billow = fbm(
                p * 0.58 + float2(drift.x * 0.45, -drift.y * 0.3) + seedOffset,
                detail
            ) - 0.5;
            float bend = sin(p.y * 0.72 - time * flow * 0.85 + seedOffset) * turbulence;
            float phase = p.x * 2.75 + bend + billow * (2.0 + turbulence * 3.1);
            float broadFold = 0.5 + 0.5 * sin(phase);
            float fineFold = 0.5 + 0.5 * sin(
                phase * 2.15 - p.y * 0.62 + time * flow * 1.35
            );
            float grazingLight = pow(
                max(0.0, 1.0 - abs(cos(phase + 0.7))),
                mix(5.5, 1.8, softness)
            );
            intensity = broadFold * 0.58 + fineFold * 0.14;
            intensity += grazingLight * (0.28 + glow * 0.44);
            intensity *= 0.82 + billow * 0.42;
        } else {
            // Several refracted wave fronts meet in narrow, luminous ridges.
            // Low-frequency noise keeps the lattice suspended in a soft veil.
            float2 p = uv * structure * 1.55;
            float haze = fbm(p * 0.38 - drift * 0.35 + seedOffset, detail);
            float2 warped = p + float2(
                sin(p.y * 0.74 + time * flow + seedOffset),
                cos(p.x * 0.67 - time * flow * 0.83 - seedOffset)
            ) * turbulence * 0.42;
            warped += (haze - 0.5) * turbulence * 0.72;
            float waveA = sin(warped.x * 2.25 + sin(warped.y * 1.32 + time * flow * 2.0));
            float waveB = sin(warped.y * 2.48 - cos(warped.x * 1.18 - time * flow * 1.55));
            float waveC = sin((warped.x + warped.y) * 1.26 + time * flow * 0.92);
            float intersection = abs(waveA + waveB + waveC * 0.58) / 2.58;
            float ridge = pow(
                clamp(1.0 - intersection, 0.0, 1.0),
                mix(9.0, 2.2, softness)
            );
            intensity = haze * 0.28 + ridge * (0.72 + glow * 0.58);
            intensity += pow(ridge, 2.0) * glow * 0.34;
        }

        intensity *= 0.84 + energy * 0.52;

        // Every numeric art parameter contributes to a normalized wavelength.
        // Local field intensity then shifts that wavelength across the border,
        // so a selected spectrum is a living range rather than a palette swatch.
        float structureN = clamp((structure - 0.5) / 2.5, 0.0, 1.0);
        float detailN = clamp((detail - 0.15) / 0.60, 0.0, 1.0);
        float turbulenceN = clamp(turbulence / 2.5, 0.0, 1.0);
        float softnessN = clamp((softness - 0.15) / 0.70, 0.0, 1.0);
        float flowN = clamp(flow / 0.35, 0.0, 1.0);
        float formDrive = (structureN + detailN + softnessN) / 3.0;
        float reactivity = turbulenceN * 0.30
            + flowN * 0.25
            + glow * 0.22
            + energy * 0.23;
        float parameterDrive = formDrive * 0.38 + reactivity * 0.62;
        float localDrive = clamp(intensity, 0.0, 1.0);
        float wavelength = clamp(
            parameterDrive * 0.72
                + localDrive * 0.20
                + reactivity * localDrive * 0.18,
            0.0,
            1.0
        );

        float3 effectColor = spectrumRamp(wavelength, spectrumIndex);
        float highlightWavelength = min(1.0, wavelength + 0.08 + reactivity * 0.10);
        float highlightMix = clamp(localDrive * 0.28 + glow * 0.16, 0.0, 0.48);
        effectColor = mix(
            effectColor,
            spectrumRamp(highlightWavelength, spectrumIndex),
            highlightMix
        );

        effectColor = applyPost(
            effectColor,
            postEffect,
            postAmount,
            uv,
            in.position.xy,
            time
        );

        if (ditherMode == 1) {
            effectColor = ditheredQuantization(
                effectColor,
                bayer4(in.position.xy),
                ditherRatio
            );
        } else if (ditherMode == 2) {
            float threshold = interleavedGradientNoise(in.position.xy);
            effectColor = ditheredQuantization(effectColor, threshold, ditherRatio);
        }

        float edgeDistance = min(
            min(in.position.x, resolution.x - in.position.x),
            min(in.position.y, resolution.y - in.position.y)
        );
        float reach = max(1.0, min(resolution.x, resolution.y) * u.composite.w);

        // Reach marks the maximum slope of the inner boundary, not a clip.
        // A normalized logistic keeps the screen edge fully illuminated while
        // allowing the material to spill naturally beyond the precipice.
        float falloffWidth = reach * (
            0.28 + softness * 0.55 + energy * 0.20
        );
        float borderMask = 1.0 / (
            1.0 + exp((edgeDistance - reach) / max(falloffWidth, 0.001))
        );
        float edgeNormalization = 1.0 / (
            1.0 + exp(-reach / max(falloffWidth, 0.001))
        );
        borderMask = clamp(borderMask / max(edgeNormalization, 0.001), 0.0, 1.0);

        float luminosity = borderMask
            * (0.22 + intensity * 0.82)
            * (0.72 + glow * 0.52 + energy * 0.28);
        float3 background = clamp(u.composite.rgb, 0.0, 1.0);
        float3 lightLayer = clamp(effectColor * luminosity, 0.0, 1.0);
        float3 color = 1.0 - (1.0 - background) * (1.0 - lightLayer);
        color = clamp(color, 0.0, 1.0);
        return half4(half3(color), 1.0h);
    }
    """#
}
