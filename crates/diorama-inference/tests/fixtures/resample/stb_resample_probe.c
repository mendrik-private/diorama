/* Development-only fixture generator; never compiled by the Rust crate.
 * Build against stb_image_resize.h and capture the printed vectors in
 * resample.rs tests. */
#define STB_IMAGE_RESIZE_IMPLEMENTATION
#include "stb_image_resize.h"
#include <stdio.h>
int main(void) {
  unsigned char rgba[] = {255,0,0,0, 0,255,0,128, 0,0,255,255,
                          200,100,50,64, 20,40,60,255, 255,255,0,32};
  float mask[] = {0,.1f,.9f,1,.4f,.7f};
  unsigned char out[64], down[8], one_axis[60], narrow[80]; float mask_out[16];
  unsigned char one_pixel[] = {255,0,0,0, 0,255,0,128, 0,0,255,255};
  stbir_resize_uint8_generic(rgba,3,2,0,out,4,4,0,4,3,0,STBIR_EDGE_CLAMP,STBIR_FILTER_DEFAULT,STBIR_COLORSPACE_SRGB,0);
  stbir_resize_float_generic(mask,3,2,0,mask_out,4,4,0,1,-1,0,STBIR_EDGE_CLAMP,STBIR_FILTER_DEFAULT,STBIR_COLORSPACE_LINEAR,0);
  for (int i=0;i<64;i++) printf("%u%s",out[i],i==63?"\n":",");
  for (int i=0;i<16;i++) printf("%.9g%s",mask_out[i],i==15?"\n":",");
  stbir_resize_uint8_generic(rgba,3,2,0,down,2,1,0,4,3,0,STBIR_EDGE_CLAMP,STBIR_FILTER_DEFAULT,STBIR_COLORSPACE_SRGB,0);
  stbir_resize_uint8_generic(rgba,3,2,0,one_axis,3,5,0,4,3,0,STBIR_EDGE_CLAMP,STBIR_FILTER_DEFAULT,STBIR_COLORSPACE_SRGB,0);
  for (int i=0;i<8;i++) printf("%u%s",down[i],i==7?"\n":",");
  for (int i=0;i<60;i++) printf("%u%s",one_axis[i],i==59?"\n":",");
  stbir_resize_uint8_generic(one_pixel,1,3,0,narrow,4,5,0,4,3,0,STBIR_EDGE_CLAMP,STBIR_FILTER_DEFAULT,STBIR_COLORSPACE_SRGB,0);
  for (int i=0;i<80;i++) printf("%u%s",narrow[i],i==79?"\n":",");
}
