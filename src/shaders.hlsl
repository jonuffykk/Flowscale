cbuffer C : register(b0) {
    float4 g_src;
    float4 g_dst;
    float4 g_p0;
    float4 g_p1;
    int4   g_i0;
    int4   g_txt[4];
};

Texture2D<float4> tSrc : register(t0);
Texture2D<float4> tAux : register(t1);
SamplerState sLin : register(s0);
SamplerState sPt  : register(s1);

float4 VS(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float2 UV(float4 pos) { return (pos.xy - g_dst.xy) / g_dst.zw; }
float  Luma(float3 c) { return dot(c, float3(0.299, 0.587, 0.114)); }

// Variable rate scaling: g_i0.w != 0 means "only redraw what changed". tDirty holds one flag per
// 16x16 source tile (already widened by the filters' reach); unchanged pixels keep last frame's output.
Texture2D<float> tDirty : register(t2);
bool VrsSkip(float2 uv) {
    if (g_i0.w == 0) return false;
    int2 t = int2(uv * g_src.xy) / 16;
    return tDirty.Load(int3(t, 0)) < 0.5;
}

float4 PS_Nearest(float4 pos : SV_Position) : SV_Target { if (VrsSkip(UV(pos))) discard; return float4(tSrc.SampleLevel(sPt, UV(pos), 0).rgb, 1); }
float4 PS_Bilinear(float4 pos : SV_Position) : SV_Target { if (VrsSkip(UV(pos))) discard; return float4(tSrc.SampleLevel(sLin, UV(pos), 0).rgb, 1); }

float4 PS_SharpBilinear(float4 pos : SV_Position) : SV_Target {
    if (VrsSkip(UV(pos))) discard;
    float2 tx = UV(pos) * g_src.xy;
    float2 scale = max(g_dst.zw * g_src.zw, 1.0);
    float2 i = floor(tx - 0.5) + 0.5;
    float2 f = tx - i;
    float2 reg = 0.5 - 0.5 / scale;
    f = saturate((f - reg) / max(1.0 - 2.0 * reg, 1e-5));
    return float4(tSrc.SampleLevel(sLin, (i + f) * g_src.zw, 0).rgb, 1);
}

float4 PS_Bicubic(float4 pos : SV_Position) : SV_Target {
    if (VrsSkip(UV(pos))) discard;
    float2 sp = UV(pos) * g_src.xy;
    float2 t1 = floor(sp - 0.5) + 0.5, f = sp - t1;
    float2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    float2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    float2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    float2 w3 = f * f * (-0.5 + 0.5 * f);
    float2 w12 = w1 + w2, o12 = w2 / w12;
    float2 p0 = (t1 - 1) * g_src.zw, p3 = (t1 + 2) * g_src.zw, p12 = (t1 + o12) * g_src.zw;
    float3 c = 0;
    c += tSrc.SampleLevel(sLin, float2(p0.x,  p0.y),  0).rgb * w0.x  * w0.y;
    c += tSrc.SampleLevel(sLin, float2(p12.x, p0.y),  0).rgb * w12.x * w0.y;
    c += tSrc.SampleLevel(sLin, float2(p3.x,  p0.y),  0).rgb * w3.x  * w0.y;
    c += tSrc.SampleLevel(sLin, float2(p0.x,  p12.y), 0).rgb * w0.x  * w12.y;
    c += tSrc.SampleLevel(sLin, float2(p12.x, p12.y), 0).rgb * w12.x * w12.y;
    c += tSrc.SampleLevel(sLin, float2(p3.x,  p12.y), 0).rgb * w3.x  * w12.y;
    c += tSrc.SampleLevel(sLin, float2(p0.x,  p3.y),  0).rgb * w0.x  * w3.y;
    c += tSrc.SampleLevel(sLin, float2(p12.x, p3.y),  0).rgb * w12.x * w3.y;
    c += tSrc.SampleLevel(sLin, float2(p3.x,  p3.y),  0).rgb * w3.x  * w3.y;
    return float4(max(c, 0), 1);
}

float3 Px(int2 p) { return tSrc.Load(int3(clamp(p, int2(0, 0), int2(g_src.xy) - 1), 0)).rgb; }

float L3(float x) {
    x = abs(x); if (x < 1e-4) return 1; if (x >= 3) return 0;
    float px = 3.14159265 * x; return 3.0 * sin(px) * sin(px / 3.0) / (px * px);
}
float4 PS_Lanczos(float4 pos : SV_Position) : SV_Target {
    if (VrsSkip(UV(pos))) discard;
    float2 p = UV(pos) * g_src.xy - 0.5, b = floor(p), f = p - b;
    float3 acc = 0; float ws = 0;
    [unroll] for (int y = -2; y <= 3; y++) {
        float wy = L3(y - f.y);
        [unroll] for (int x = -2; x <= 3; x++) {
            float w = L3(x - f.x) * wy; acc += Px(int2(b) + int2(x, y)) * w; ws += w;
        }
    }
    float3 c = acc / ws;
    float3 a = Px(int2(b)), bb = Px(int2(b) + int2(1, 0)), cc = Px(int2(b) + int2(0, 1)), d = Px(int2(b) + 1);
    return float4(clamp(c, min(min(a, bb), min(cc, d)), max(max(a, bb), max(cc, d))), 1);
}

float EL(float3 c) { return c.g + 0.5 * (c.r + c.b); }
void EasuSet(inout float2 dir, inout float len, float w, float lA, float lB, float lC, float lD, float lE) {
    float dc = lD - lC, cb = lC - lB;
    float lx = max(abs(dc), abs(cb)); lx = lx > 0 ? 1.0 / lx : 0;
    float dx = lD - lB; lx = saturate(abs(dx) * lx); lx *= lx;
    float ec = lE - lC, ca = lC - lA;
    float ly = max(abs(ec), abs(ca)); ly = ly > 0 ? 1.0 / ly : 0;
    float dy = lE - lA; ly = saturate(abs(dy) * ly); ly *= ly;
    dir += float2(dx, dy) * w; len += (lx + ly) * w;
}
void EasuTap(inout float3 aC, inout float aW, float2 off, float2 dir, float2 len2, float lob, float clp, float3 c) {
    float2 v = float2(off.x * dir.x + off.y * dir.y, off.x * -dir.y + off.y * dir.x) * len2;
    float d2 = min(dot(v, v), clp);
    float wB = 0.4 * d2 - 1.0, wA = lob * d2 - 1.0;
    wB *= wB; wA *= wA; wB = 1.5625 * wB - 0.5625;
    float w = wB * wA; aC += c * w; aW += w;
}
float4 Easu(float4 pos, bool fast) {
    if (VrsSkip(UV(pos))) discard;
    float2 pp = UV(pos) * g_src.xy - 0.5, fp = floor(pp); pp -= fp;
    int2 o = int2(fp);
    float3 b = Px(o + int2(0, -1)), c = Px(o + int2(1, -1));
    float3 e = Px(o + int2(-1, 0)), f = Px(o), g = Px(o + int2(1, 0)), h = Px(o + int2(2, 0));
    float3 i = Px(o + int2(-1, 1)), j = Px(o + int2(0, 1)), k = Px(o + int2(1, 1)), l = Px(o + int2(2, 1));
    float3 n = Px(o + int2(0, 2)), oo = Px(o + int2(1, 2));
    float bL = EL(b), cL = EL(c), eL = EL(e), fL = EL(f), gL = EL(g), hL = EL(h);
    float iL = EL(i), jL = EL(j), kL = EL(k), lL = EL(l), nL = EL(n), oL = EL(oo);
    float2 dir = 0; float len = 0;
    if (fast) {
        // Performance variant: one gradient analysis on the nearest texel instead of the bilinear 2x2 quad.
        float2 q = round(pp);
        if (q.x < 0.5 && q.y < 0.5) EasuSet(dir, len, 1.0, bL, eL, fL, gL, jL);
        else if (q.y < 0.5)         EasuSet(dir, len, 1.0, cL, fL, gL, hL, kL);
        else if (q.x < 0.5)         EasuSet(dir, len, 1.0, fL, iL, jL, kL, nL);
        else                        EasuSet(dir, len, 1.0, gL, jL, kL, lL, oL);
    } else {
        EasuSet(dir, len, (1 - pp.x) * (1 - pp.y), bL, eL, fL, gL, jL);
        EasuSet(dir, len, pp.x * (1 - pp.y),       cL, fL, gL, hL, kL);
        EasuSet(dir, len, (1 - pp.x) * pp.y,       fL, iL, jL, kL, nL);
        EasuSet(dir, len, pp.x * pp.y,             gL, jL, kL, lL, oL);
    }
    float dr = dot(dir, dir); bool zro = dr < 1.0 / 32768.0;
    dir = zro ? float2(1, 0) : dir * rsqrt(dr);
    len = len * 0.5; len *= len;
    float stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    float2 len2 = float2(1 + (stretch - 1) * len, 1 - 0.5 * len);
    float lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len, clp = 1.0 / lob;
    float3 aC = 0; float aW = 0;
    EasuTap(aC, aW, float2( 0, -1) - pp, dir, len2, lob, clp, b);
    EasuTap(aC, aW, float2( 1, -1) - pp, dir, len2, lob, clp, c);
    EasuTap(aC, aW, float2(-1,  1) - pp, dir, len2, lob, clp, i);
    EasuTap(aC, aW, float2( 0,  1) - pp, dir, len2, lob, clp, j);
    EasuTap(aC, aW, float2( 0,  0) - pp, dir, len2, lob, clp, f);
    EasuTap(aC, aW, float2(-1,  0) - pp, dir, len2, lob, clp, e);
    EasuTap(aC, aW, float2( 1,  1) - pp, dir, len2, lob, clp, k);
    EasuTap(aC, aW, float2( 2,  1) - pp, dir, len2, lob, clp, l);
    EasuTap(aC, aW, float2( 2,  0) - pp, dir, len2, lob, clp, h);
    EasuTap(aC, aW, float2( 1,  0) - pp, dir, len2, lob, clp, g);
    EasuTap(aC, aW, float2( 1,  2) - pp, dir, len2, lob, clp, oo);
    EasuTap(aC, aW, float2( 0,  2) - pp, dir, len2, lob, clp, n);
    float3 mn = min(min(f, g), min(j, k)), mx = max(max(f, g), max(j, k));
    return float4(clamp(aC / aW, mn, mx), 1);
}
float4 PS_Easu(float4 pos : SV_Position) : SV_Target { return Easu(pos, false); }
float4 PS_EasuFast(float4 pos : SV_Position) : SV_Target { return Easu(pos, true); }

// ---------------------------------------------------------------- Snapdragon GSR 1 (spatial)
// Ported from Qualcomm sgsr1_mobile.h (BSD-3-Clause, licenses/LICENSE-snapdragon-gsr.txt):
// RGBA mode, edge-direction weighting on, green channel as the luma proxy.
float FastLanczos2(float x) { float wA = x - 4.0; float wB = x * wA - wA; wA *= wA; return wB * wA; }
float2 SgsrWeight(float dx, float dy, float c, float3 data) {
    float std = data.x; float2 dir = data.yz;
    float edgeDis = dx * dir.y + dy * dir.x;
    float x = (dx * dx + dy * dy) + (edgeDis * edgeDis) * (saturate(c * c * std) * 0.7 - 1.0);
    float w = FastLanczos2(x);
    return float2(w, w * c);
}
float2 SgsrDir(float4 l, float4 r) {
    float RxLz = r.x - l.z, RwLy = r.w - l.y;
    float2 d = float2(RxLz + RwLy, RxLz - RwLy);
    return d * rsqrt(d.x * d.x + 3.075740e-05 + d.y * d.y);
}
float4 PS_Sgsr(float4 pos : SV_Position) : SV_Target {
    if (VrsSkip(UV(pos))) discard;
    float2 uv = UV(pos);
    float4 con = float4(g_src.zw, g_src.xy);
    float4 pix = float4(tSrc.SampleLevel(sLin, uv, 0).rgb, 0);
    float2 imgCoord = uv * con.zw + float2(-0.5, 0.5);
    float2 ip = floor(imgCoord);
    float2 coord = ip * con.xy;
    float2 pl = imgCoord - ip;
    float4 left = tSrc.GatherGreen(sLin, coord);
    float edgeVote = abs(left.z - left.y) + abs(pix.g - left.y) + abs(pix.g - left.z);
    if (edgeVote > 8.0 / 255.0) {
        coord.x += con.x;
        float4 right = tSrc.GatherGreen(sLin, coord + float2(con.x, 0));
        float4 ud;
        ud.xy = tSrc.GatherGreen(sLin, coord + float2(0, -con.y)).wz;
        ud.zw = tSrc.GatherGreen(sLin, coord + float2(0, con.y)).yx;
        float mean = (left.y + left.z + right.x + right.w) * 0.25;
        left -= mean; right -= mean; ud -= mean;
        pix.w = pix.g - mean;
        float sum = dot(abs(left), 1) + dot(abs(right), 1) + dot(abs(ud), 1);
        float sm = 10.14185 / max(sum, 1e-6);
        float3 data = float3(sm * sm, SgsrDir(left, right));
        float2 a = SgsrWeight(pl.x, pl.y + 1, ud.x, data);
        a += SgsrWeight(pl.x - 1, pl.y + 1, ud.y, data);
        a += SgsrWeight(pl.x - 1, pl.y - 2, ud.z, data);
        a += SgsrWeight(pl.x, pl.y - 2, ud.w, data);
        a += SgsrWeight(pl.x + 1, pl.y - 1, left.x, data);
        a += SgsrWeight(pl.x, pl.y - 1, left.y, data);
        a += SgsrWeight(pl.x, pl.y, left.z, data);
        a += SgsrWeight(pl.x + 1, pl.y, left.w, data);
        a += SgsrWeight(pl.x - 1, pl.y - 1, right.x, data);
        a += SgsrWeight(pl.x - 2, pl.y - 1, right.y, data);
        a += SgsrWeight(pl.x - 2, pl.y, right.z, data);
        a += SgsrWeight(pl.x - 1, pl.y, right.w, data);
        float y = a.y / a.x;
        float mx4 = max(max(left.y, left.z), max(right.x, right.w)), mn4 = min(min(left.y, left.z), min(right.x, right.w));
        y = clamp(2.0 * y, mn4, mx4);
        pix.rgb = (g_p0.w > 0.5) ? max(pix.rgb + (y - pix.w), 0) : saturate(pix.rgb + (y - pix.w));
    }
    return float4(pix.rgb, 1);
}

// ---------------------------------------------------------------- xBR level 2 (pixel art)
// Ported from Hyllian xbr-lv2 (MIT, licenses/LICENSE-xbr.txt): corner mode C, anti-aliased blending.
static const float3 XY = float3(0.2627, 0.6780, 0.0593);
static const float3 V2F = float3(65536, 256, 1);
float  XDist(float3 a, float3 b) { return dot(abs(a - b), XY); }
float4 XDist4(float4x3 a, float4x3 b) { return float4(XDist(a[0], b[0]), XDist(a[1], b[1]), XDist(a[2], b[2]), XDist(a[3], b[3])); }
float4 XEq(float4x3 a, float4x3 b) { return step(XDist4(a, b), 0.32); }
float4 XNeq(float4x3 a, float4x3 b) { return 1.0 - XEq(a, b); }
float4 XEnc(float4x3 m) { return float4(dot(V2F, m[0]), dot(V2F, m[1]), dot(V2F, m[2]), dot(V2F, m[3])); }
float4 XDiff(float4 a, float4 b) { return float4(a != b); }
float4 XWd(float4x3 a, float4x3 b, float4x3 c, float4x3 d, float4x3 e, float4x3 f, float4x3 g, float4x3 h) {
    return XDist4(a, b) + XDist4(a, c) + XDist4(d, e) + XDist4(d, f) + 4.0 * XDist4(g, h);
}
float4 PS_Xbr(float4 pos : SV_Position) : SV_Target {
    if (VrsSkip(UV(pos))) discard;
    float2 sp = UV(pos) * g_src.xy;
    int2 o = int2(floor(sp));
    float2 fp = frac(sp);
    float3 A1 = Px(o + int2(-1, -2)), B1 = Px(o + int2(0, -2)), C1 = Px(o + int2(1, -2));
    float3 A = Px(o + int2(-1, -1)), B = Px(o + int2(0, -1)), C = Px(o + int2(1, -1));
    float3 D = Px(o + int2(-1, 0)), E = Px(o), F = Px(o + int2(1, 0));
    float3 G = Px(o + int2(-1, 1)), H = Px(o + int2(0, 1)), I = Px(o + int2(1, 1));
    float3 G5 = Px(o + int2(-1, 2)), H5 = Px(o + int2(0, 2)), I5 = Px(o + int2(1, 2));
    float3 A0 = Px(o + int2(-2, -1)), D0 = Px(o + int2(-2, 0)), G0 = Px(o + int2(-2, 1));
    float3 C4 = Px(o + int2(2, -1)), F4 = Px(o + int2(2, 0)), I4 = Px(o + int2(2, 1));
    float4x3 b = float4x3(B, D, H, F), c = float4x3(C, A, G, I), d = float4x3(D, H, F, B), e = float4x3(E, E, E, E);
    float4x3 f = float4x3(F, B, D, H), g = float4x3(G, I, C, A), h = float4x3(H, F, B, D), i = float4x3(I, C, A, G);
    float4x3 i4 = float4x3(I4, C1, A0, G5), i5 = float4x3(I5, C4, A1, G0), h5 = float4x3(H5, F4, B1, D0), f4 = float4x3(F4, B1, D0, H5);
    float4 b_ = XEnc(b), c_ = XEnc(c), d_ = b_.yzwx, e_ = XEnc(e), f_ = b_.wxyz, g_ = c_.zwxy, h_ = b_.zwxy;
    const float4 Ao = float4(1, -1, -1, 1), Bo = float4(1, 1, -1, -1), Co = float4(1.5, 0.5, -0.5, 0.5);
    const float4 Ax = float4(1, -1, -1, 1), Bx = float4(0.5, 2, -0.5, -2), Cx = float4(1, 1, -0.5, 0);
    const float4 Ay = float4(1, -1, -1, 1), By = float4(2, 0.5, -2, -0.5), Cy = float4(2, 0, -1, 0.5);
    const float4 Ci = 0.25;
    float4 fx = Ao * fp.y + Bo * fp.x, fx_l = Ax * fp.y + Bx * fp.x, fx_u = Ay * fp.y + By * fp.x;
    float4 irlv0 = XDiff(e_, f_) * XDiff(e_, h_);
    float4 irlv1 = saturate(irlv0 * (XNeq(f, b) * XNeq(f, c) + XNeq(h, d) * XNeq(h, g) + XEq(e, i) * (XNeq(f, f4) * XNeq(f, i4) + XNeq(h, h5) * XNeq(h, i5)) + XEq(e, g) + XEq(e, c)));
    float4 irlv2l = XDiff(e_, g_) * XDiff(d_, g_), irlv2u = XDiff(e_, c_) * XDiff(b_, c_);
    float aa = 2.0 * g_src.x / g_dst.z;
    float4 deltaL = float4(0.5, 1, 0.5, 1) * aa, deltaU = deltaL.yxwz;
    float4 fx45i = saturate(0.5 + (fx - Co - Ci) / aa), fx45 = saturate(0.5 + (fx - Co) / aa);
    float4 fx30 = saturate(0.5 + (fx_l - Cx) / deltaL), fx60 = saturate(0.5 + (fx_u - Cy) / deltaU);
    float4 wd1 = XWd(e, c, g, i, h5, f4, h, f), wd2 = XWd(h, d, i5, f, i4, b, e, i);
    float4 d_fg = XDist4(f, g), d_hc = XDist4(h, c);
    float4 edri = step(wd1, wd2) * irlv0;
    float4 edr = float4(wd1 < wd2) * irlv1 * (1.0 - edri.yzwx * edri.wxyz);
    float4 edr_l = step(2.3 * d_fg, d_hc) * irlv2l * edr * ((1.0 - edri.yzwx) * XEq(e, c));
    float4 edr_u = step(2.3 * d_hc, d_fg) * irlv2u * edr * ((1.0 - edri.wxyz) * XEq(e, g));
    fx45i *= edri; fx45 *= edr; fx30 *= edr_l; fx60 *= edr_u;
    float4 px = step(XDist4(e, f), XDist4(e, h));
    float4 m = max(max(fx30, fx60), max(fx45, fx45i));
    float3 r1 = lerp(E, lerp(H, F, px.x), m.x), r2 = lerp(E, lerp(B, D, px.z), m.z);
    float3 ra = lerp(r1, r2, step(XDist(E, r1), XDist(E, r2)));
    r1 = lerp(E, lerp(F, B, px.y), m.y); r2 = lerp(E, lerp(D, H, px.w), m.w);
    float3 rb = lerp(r1, r2, step(XDist(E, r1), XDist(E, r2)));
    return float4(lerp(ra, rb, step(XDist(E, ra), XDist(E, rb))), 1);
}

// ---------------------------------------------------------------- AMD FidelityFX CAS (sharpen-only path)
// Contrast Adaptive Sharpening after ffx_cas.h CasFilter without scaling (MIT, licenses/LICENSE-FidelityFX-CAS.txt).
float4 PS_Cas(float4 pos : SV_Position) : SV_Target {
    int2 p = int2(pos.xy - g_dst.xy);
    float3 a = tSrc.Load(int3(p + int2(-1, -1), 0)).rgb, b = tSrc.Load(int3(p + int2(0, -1), 0)).rgb, c = tSrc.Load(int3(p + int2(1, -1), 0)).rgb;
    float3 d = tSrc.Load(int3(p + int2(-1, 0), 0)).rgb, e = tSrc.Load(int3(p, 0)).rgb, f = tSrc.Load(int3(p + int2(1, 0), 0)).rgb;
    float3 g = tSrc.Load(int3(p + int2(-1, 1), 0)).rgb, h = tSrc.Load(int3(p + int2(0, 1), 0)).rgb, i = tSrc.Load(int3(p + int2(1, 1), 0)).rgb;
    float k = (g_p0.w > 0.5) ? max(1, max(max(e.r, e.g), e.b)) : 1;
    a /= k; b /= k; c /= k; d /= k; e /= k; f /= k; g /= k; h /= k; i /= k;
    float3 mn = min(min(min(d, e), min(f, b)), h), mn2 = min(min(min(mn, a), min(c, g)), i); mn += mn2;
    float3 mx = max(max(max(d, e), max(f, b)), h), mx2 = max(max(max(mx, a), max(c, g)), i); mx += mx2;
    float3 amp = sqrt(saturate(min(mn, 2.0 - mx) / max(mx, 1e-5)));
    float peak = -1.0 / lerp(8.0, 5.0, saturate(g_p0.x));
    float3 w = amp * peak;
    float3 o = ((b + d + f + h) * w + e) / (1.0 + 4.0 * w);
    return float4(max(o, 0) * k, 1);
}

// ---------------------------------------------------------------- Desktop Duplication on a rotated display
// g_i0.x = DXGI_MODE_ROTATION (2: 90, 3: 180, 4: 270); the source is the unrotated desktop surface.
float4 PS_Rotate(float4 pos : SV_Position) : SV_Target {
    float2 uv = UV(pos), r = uv;
    // The duplicated image is the desktop turned clockwise by the rotation; sample it back upright.
    if (g_i0.x == 2) r = float2(1 - uv.y, uv.x);
    else if (g_i0.x == 3) r = 1 - uv;
    else if (g_i0.x == 4) r = float2(uv.y, 1 - uv.x);
    return float4(tSrc.SampleLevel(sPt, r, 0).rgb, 1);
}


float4 PS_Line(float4 pos : SV_Position) : SV_Target {
    if (VrsSkip(UV(pos))) discard;
    float2 uv = UV(pos), d = 1.0 / g_dst.zw;
    float3 c[9]; float l[9];
    [unroll] for (int y = 0; y < 3; y++) [unroll] for (int x = 0; x < 3; x++) {
        c[y * 3 + x] = tSrc.SampleLevel(sLin, uv + float2(x - 1, y - 1) * d, 0).rgb; l[y * 3 + x] = Luma(c[y * 3 + x]);
    }
    float3 o = c[4]; float s = 0.6;
    int3 A[4] = { int3(0,1,2), int3(0,3,6), int3(1,2,5), int3(0,1,3) };
    int3 B[4] = { int3(6,7,8), int3(2,5,8), int3(3,6,7), int3(5,7,8) };
    [unroll] for (int k = 0; k < 4; k++) {
        float aMin = min(l[A[k].x], min(l[A[k].y], l[A[k].z])), aMax = max(l[A[k].x], max(l[A[k].y], l[A[k].z]));
        float bMin = min(l[B[k].x], min(l[B[k].y], l[B[k].z])), bMax = max(l[B[k].x], max(l[B[k].y], l[B[k].z]));
        float3 aAvg = (c[A[k].x] + c[A[k].y] + c[A[k].z]) / 3, bAvg = (c[B[k].x] + c[B[k].y] + c[B[k].z]) / 3;
        if (aMin > max(l[4], bMax)) o = lerp(o, aAvg, s * 0.5);
        else if (bMin > max(l[4], aMax)) o = lerp(o, bAvg, s * 0.5);
    }
    return float4(o, 1);
}

float4 PS_Rcas(float4 pos : SV_Position) : SV_Target {
    int2 p = int2(pos.xy - g_dst.xy);
    float3 b = tSrc.Load(int3(p + int2(0, -1), 0)).rgb, d = tSrc.Load(int3(p + int2(-1, 0), 0)).rgb;
    float3 e = tSrc.Load(int3(p, 0)).rgb;
    float3 f = tSrc.Load(int3(p + int2(1, 0), 0)).rgb, h = tSrc.Load(int3(p + int2(0, 1), 0)).rgb;
    if (g_p0.w > 0.5) { float m = max(1, max(max(e.r, e.g), e.b)); b /= m; d /= m; e /= m; f /= m; h /= m; }
    float3 mn4 = min(min(b, d), min(f, h)), mx4 = max(max(b, d), max(f, h));
    float3 hitMin = min(mn4, e) / (4.0 * mx4 + 1e-5);
    float3 hitMax = (1.0 - max(mx4, e)) / (4.0 * mn4 - 4.0 - 1e-5);
    float3 lobeRGB = max(-hitMin, hitMax);
    float sharp = exp2(-(1.0 - g_p0.x) * 2.0);
    float lobe = max(-0.1875, min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0)) * sharp;
    float3 o = (lobe * (b + d + f + h) + e) / (4.0 * lobe + 1.0);
    if (g_p0.w > 0.5) o *= max(1, max(max(tSrc.Load(int3(p, 0)).r, tSrc.Load(int3(p, 0)).g), tSrc.Load(int3(p, 0)).b));
    return float4(o, 1);
}

float4 PS_Cursor(float4 pos : SV_Position) : SV_Target {
    float4 c = tSrc.SampleLevel(sPt, UV(pos), 0);
    if (g_p0.w > 0.5) c.rgb *= 2.5;
    return c;
}
// In-game overlay panel: premultiplied BGRA rendered on the CPU (osd.rs).
float4 PS_Overlay(float4 pos : SV_Position) : SV_Target {
    float4 c = tSrc.SampleLevel(sPt, UV(pos), 0);
    if (g_p0.w > 0.5) { float a = max(c.a, 1e-4); c.rgb = pow(saturate(c.rgb / a), 2.2) * a * 2.5; }
    return c;
}

Texture2D<float>  tL0 : register(t0);
Texture2D<float>  tL1 : register(t1);
Texture2D<float2> tF0 : register(t2);
Texture2D<float2> tF1 : register(t3);
RWTexture2D<float>  uLuma : register(u0);
RWTexture2D<float2> uFlow : register(u0);
RWTexture2D<float4> uOut  : register(u0);

// One thread per 16x16 tile of the captured frame: flag it when any pixel within the tile plus a 4 px
// margin (the widest filter reach, Lanczos 3 / xBR) differs from the previous frame.
[numthreads(8, 8, 1)] void CS_Dirty(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= (uint2)g_p1.yz)) return;
    int2 o = int2(id.xy) * 16 - 4;
    float d = 0;
    for (int y = 0; y < 24 && d == 0; y++)
        for (int x = 0; x < 24; x++) {
            int3 q = int3(clamp(o + int2(x, y), int2(0, 0), int2(g_src.xy) - 1), 0);
            float3 a = tSrc.Load(q).rgb, b = tAux.Load(q).rgb;
            if (any(abs(a - b) > 0.5 / 255.0)) { d = 1; break; }
        }
    uLuma[id.xy] = d;
}

[numthreads(8, 8, 1)] void CS_Luma(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= (uint2)g_p1.yz)) return;
    float2 uv = (id.xy + 0.5) / g_p1.yz;
    float l = Luma(tSrc.SampleLevel(sLin, uv, 0).rgb);
    if (g_p0.w > 0.5) l = l / (1 + l);
    uLuma[id.xy] = l;
}
[numthreads(8, 8, 1)] void CS_Down(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= (uint2)g_p1.yz)) return;
    int2 s = id.xy * 2, m = int2(g_p1.yz * 2) - 1;
    uLuma[id.xy] = 0.25 * (tL0.Load(int3(min(s, m), 0)) + tL0.Load(int3(min(s + int2(1, 0), m), 0)) +
                           tL0.Load(int3(min(s + int2(0, 1), m), 0)) + tL0.Load(int3(min(s + 1, m), 0)));
}

float Cost(int2 p, float2 v, float pa[25], const int PR) {
    float s = 0; float2 inv = 1.0 / g_p1.yz; float2 base = (p + 0.5 + v) * inv;
    [unroll] for (int j = 0; j < (2 * PR + 1) * (2 * PR + 1); j++) {
        float2 o = float2(j % (2 * PR + 1) - PR, j / (2 * PR + 1) - PR);
        s += abs(pa[j] - tL1.SampleLevel(sLin, base + o * inv, 0));
    }
    return s;
}
void FlowMain(uint3 id, const int PR) {
    if (any(id.xy >= (uint2)g_p1.yz)) return;
    int2 p = id.xy; float2 uv = (p + 0.5) / g_p1.yz;
    float2 init = 0;
    float2 ci = g_p1.yz; float2 k = 1;
    if (g_i0.x == 1) { tF0.GetDimensions(ci.x, ci.y); k = g_p1.yz / ci; init = tF0.SampleLevel(sLin, uv, 0) * k; }
    else if (g_i0.x == 2) { init = tF0.Load(int3(p, 0)); }
    int r = g_i0.y;
    float pa[25];
    [unroll] for (int j = 0; j < (2 * PR + 1) * (2 * PR + 1); j++) pa[j] = tL0.Load(int3(clamp(p + int2(j % (2 * PR + 1) - PR, j / (2 * PR + 1) - PR), 0, int2(g_p1.yz) - 1), 0));
    float lam = g_p1.w * (2 * PR + 1) * (2 * PR + 1);
    float2 best = init; float bc = Cost(p, init, pa, PR);
    { float k0 = Cost(p, float2(0, 0), pa, PR) + lam * length(init); if (k0 < bc) { bc = k0; best = 0; } }
    if (g_i0.x != 0 && g_i0.w > 0) {
        int nd = g_i0.w == 1 ? 4 : 8;
        for (int d = 0; d < nd; d++) {
            float ang = g_i0.w == 1 ? d * 1.570796 + 0.785398 : d * 0.785398;
            float2 dir = float2(cos(ang), sin(ang));
            for (int s = (g_i0.w == 1 ? 3 : 2); s <= (g_i0.w == 1 ? 3 : 4); s += 2) {
                float2 q = uv + dir * s / ci;
                float2 cand = (g_i0.x == 1 ? tF0.SampleLevel(sLin, q, 0) * k : tF0.SampleLevel(sLin, q, 0));
                float kk = Cost(p, cand, pa, PR) + lam * length(cand - init);
                if (kk < bc) { bc = kk; best = cand; }
            }
        }
    }
    float2 ctr = best;
    for (int y = -r; y <= r; y++) for (int x = -r; x <= r; x++) {
        if (x == 0 && y == 0) continue;
        float2 v = ctr + float2(x, y);
        float k = Cost(p, v, pa, PR) + lam * length(v - init);
        if (k < bc) { bc = k; best = v; }
    }
    float cx0 = Cost(p, best - float2(1, 0), pa, PR), cx1 = Cost(p, best + float2(1, 0), pa, PR);
    float cy0 = Cost(p, best - float2(0, 1), pa, PR), cy1 = Cost(p, best + float2(0, 1), pa, PR);
    float dx = cx0 + cx1 - 2 * bc, dy = cy0 + cy1 - 2 * bc;
    if (dx > 1e-5) best.x += clamp(0.5 * (cx0 - cx1) / dx, -0.5, 0.5);
    if (dy > 1e-5) best.y += clamp(0.5 * (cy0 - cy1) / dy, -0.5, 0.5);
    uFlow[p] = best;
}
[numthreads(8, 8, 1)] void CS_Flow1(uint3 id : SV_DispatchThreadID) { FlowMain(id, 1); }
[numthreads(8, 8, 1)] void CS_Flow2(uint3 id : SV_DispatchThreadID) { FlowMain(id, 2); }
[numthreads(8, 8, 1)] void CS_Median(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= (uint2)g_p1.yz)) return;
    float2 v[9]; int2 m = int2(g_p1.yz) - 1;
    [unroll] for (int i = 0; i < 9; i++) v[i] = tF0.Load(int3(clamp(int2(id.xy) + int2(i % 3 - 1, i / 3 - 1), 0, m), 0));
    float bs = 1e30; float2 bv = v[4];
    [unroll] for (int a = 0; a < 9; a++) { float s = 0; [unroll] for (int b = 0; b < 9; b++) s += abs(v[a].x - v[b].x) + abs(v[a].y - v[b].y); if (s < bs) { bs = s; bv = v[a]; } }
    uFlow[id.xy] = bv;
}

float2 FlowAt(Texture2D<float2> t, float2 p) { return t.SampleLevel(sLin, p * g_src.zw, 0) / g_p1.x; }
float3 Prev(float2 p) { return tSrc.SampleLevel(sLin, p * g_src.zw, 0).rgb; }
float3 Cur(float2 p)  { return tAux.SampleLevel(sLin, p * g_src.zw, 0).rgb; }
float  Vis(Texture2D<float2> fw, Texture2D<float2> bw, float2 x) { float2 v = FlowAt(fw, x); float2 e = v + FlowAt(bw, x + v); return exp(-dot(e, e) * 0.05); }
float  Dif(float3 a, float3 b) { float3 d = abs(a - b); if (g_p0.w > 0.5) d /= (1 + max(a, b)); return max(d.r, max(d.g, d.b)); }

[numthreads(8, 8, 1)] void CS_Interp(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= (uint2)g_src.xy)) return;
    float2 p = id.xy + 0.5; float t = g_p0.y;
    float3 sp = tSrc.Load(int3(id.xy, 0)).rgb, sc = tAux.Load(int3(id.xy, 0)).rgb;
    float thr = g_p0.z * 0.04;
    float st = Dif(sp, sc), nb = 0;
    [unroll] for (int k = 0; k < 9; k++) {
        if (k == 4) continue;
        int2 o = int2(id.xy) + int2(k % 3 - 1, k / 3 - 1);
        nb += Dif(tSrc.Load(int3(o, 0)).rgb, tAux.Load(int3(o, 0)).rgb);
    }
    float strict = st;
    [unroll] for (int q = 0; q < 4; q++) {
        int2 o = int2(id.xy) + int2(q == 0 ? -2 : q == 1 ? 2 : 0, q == 2 ? -2 : q == 3 ? 2 : 0);
        strict = max(strict, Dif(tSrc.Load(int3(o, 0)).rgb, tAux.Load(int3(o, 0)).rgb));
    }
    float2 f00 = FlowAt(tF0, id.xy + 0.5);
    bool hud = st < thr && nb / 8 < thr * 3 && dot(f00, f00) < 2.25;
    if (thr > 0 && (strict < thr || hud)) { uOut[id.xy] = float4(lerp(sp, sc, t), 1); return; }

    float2 f = FlowAt(tF0, p); f = FlowAt(tF0, p - t * f); f = FlowAt(tF0, p - t * f);
    float2 b = FlowAt(tF1, p); b = FlowAt(tF1, p - (1 - t) * b); b = FlowAt(tF1, p - (1 - t) * b);
    float2 a0 = p - t * f, b0 = p + (1 - t) * f;
    float2 a1 = p + t * b, b1 = p - (1 - t) * b;
    float vA0 = Vis(tF0, tF1, a0), vB0 = Vis(tF1, tF0, b0), vA1 = Vis(tF0, tF1, a1), vB1 = Vis(tF1, tF0, b1);
    float3 A0 = Prev(a0), B0 = Cur(b0), A1 = Prev(a1), B1 = Cur(b1);
    float d0 = Dif(A0, B0), d1 = Dif(A1, B1);
    float3 C0 = d0 < 0.12 ? lerp(A0, B0, t) : (abs(vA0 - vB0) > 0.15 ? (vA0 > vB0 ? A0 : B0) : (t < 0.5 ? A0 : B0));
    float3 C1 = d1 < 0.12 ? lerp(A1, B1, t) : (abs(vA1 - vB1) > 0.15 ? (vA1 > vB1 ? A1 : B1) : (t < 0.5 ? A1 : B1));
    float e0 = length(f + FlowAt(tF1, b0)), e1 = length(b + FlowAt(tF0, a1));
    float w0 = exp(-e0 * e0 * 0.15) * exp(-d0 * 10) + 1e-4;
    float w1 = exp(-e1 * e1 * 0.15) * exp(-d1 * 10) + 1e-4;
    float3 col = (C0 * w0 + C1 * w1) / (w0 + w1);
    if (w0 + w1 < 0.02) col = d0 < d1 ? C0 : C1;
    uOut[id.xy] = float4(col, 1);
}
