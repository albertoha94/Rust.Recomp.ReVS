// PVRTC decoding for the native runner: wraps Imagination's PVRTDecompress
// (vendor/PVRTDecompress, MIT licence).
#include "../../../vendor/PVRTDecompress/PVRTDecompress.cpp"
extern "C" uint32_t revs_decompress_pvrtc(const void *data, bool is_2bit, uint32_t width, uint32_t height,
                                          uint8_t *rgba) {
    return pvr::PVRTDecompressPVRTC(data, is_2bit, width, height, rgba);
}
