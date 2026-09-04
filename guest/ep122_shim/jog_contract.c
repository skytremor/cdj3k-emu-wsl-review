/* SPDX-License-Identifier: MIT OR Apache-2.0 */
#include "ep122_shim.h"

const fake_modeinfo_t g_fake_mode = {
    .clock = CDJ3K_JOG_MODE_CLOCK_KHZ,
    .hdisplay = CDJ3K_JOG_MODE_HDISPLAY,
    .hsync_start = CDJ3K_JOG_MODE_HSYNC_START,
    .hsync_end = CDJ3K_JOG_MODE_HSYNC_END,
    .htotal = CDJ3K_JOG_MODE_HTOTAL,
    .hskew = CDJ3K_JOG_MODE_HSKEW,
    .vdisplay = CDJ3K_JOG_MODE_VDISPLAY,
    .vsync_start = CDJ3K_JOG_MODE_VSYNC_START,
    .vsync_end = CDJ3K_JOG_MODE_VSYNC_END,
    .vtotal = CDJ3K_JOG_MODE_VTOTAL,
    .vscan = CDJ3K_JOG_MODE_VSCAN,
    .vrefresh = CDJ3K_JOG_MODE_VREFRESH,
    .flags = CDJ3K_JOG_MODE_FLAGS,
    .type = CDJ3K_JOG_MODE_TYPE,
    .name = CDJ3K_JOG_MODE_NAME,
};

int jog_mode_contract_matches(const fake_modeinfo_t *mode)
{
    if (!mode) return 0;
    return mode->clock == g_fake_mode.clock && mode->hdisplay == g_fake_mode.hdisplay &&
           mode->hsync_start == g_fake_mode.hsync_start && mode->hsync_end == g_fake_mode.hsync_end &&
           mode->htotal == g_fake_mode.htotal && mode->vdisplay == g_fake_mode.vdisplay &&
           mode->vsync_start == g_fake_mode.vsync_start && mode->vsync_end == g_fake_mode.vsync_end &&
           mode->vtotal == g_fake_mode.vtotal && mode->vrefresh == g_fake_mode.vrefresh &&
           mode->type == g_fake_mode.type && strncmp(mode->name, g_fake_mode.name, DRM_MODE_LEN) == 0;
}

int jog_topology_contract_matches(uint32_t crtc_index, uint32_t connector_type,
                                  uint32_t connector_type_id, uint32_t possible_crtcs,
                                  uint32_t encoder_crtc_id, uint32_t requested_crtc_id)
{
    return crtc_index == CDJ3K_JOG_CRTC_INDEX &&
           connector_type == CDJ3K_JOG_CONNECTOR_TYPE &&
           connector_type_id == CDJ3K_JOG_CONNECTOR_TYPE_ID &&
           (possible_crtcs & (1u << crtc_index)) != 0 &&
           (encoder_crtc_id == 0 || encoder_crtc_id == requested_crtc_id);
}
