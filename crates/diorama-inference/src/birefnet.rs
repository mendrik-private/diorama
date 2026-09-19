//! BiRefNet tensor-layout and attention primitives.
//!
//! Adapted from vision.cpp's BiRefNet and Swin implementations, which carry
//! the MIT License:
//!
//! Copyright (c) 2025 The vision.cpp authors
//!
//! The complete adapted-source notice is retained in
//! `THIRD_PARTY_NOTICES/vision.cpp-MIT.txt`.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the "Software"),
//! to deal in the Software without restriction, including without limitation
//! the rights to use, copy, modify, merge, publish, distribute, sublicense,
//! and/or sell copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following conditions: the
//! above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software. THE SOFTWARE IS PROVIDED
//! "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED.

use std::path::Path;

use burn::tensor::{
    Int, Shape, Tensor, TensorData,
    activation::{gelu, relu, sigmoid, softmax},
    backend::Backend as BurnBackend,
    module::{adaptive_avg_pool2d, conv2d, deform_conv2d, interpolate, linear},
    ops::{ConvOptions, DeformConvOptions, InterpolateMode, InterpolateOptions, PadMode},
};
use image::{GrayImage, RgbaImage};

use crate::{Backend, gguf::GgufModel, resample};

const LAYER_NORM_EPSILON: f32 = 1e-5;

/// Runs the shipped BiRefNet GGUF model.
///
/// The model is accepted only when its metadata and every tensor range have
/// been checked.  CPU and GPU dispatch use one generic graph implementation;
/// keeping the dispatch here prevents app code from choosing a different
/// image or alpha contract per backend.
pub fn birefnet(model: &Path, image: &RgbaImage, backend: Backend) -> Result<GrayImage, String> {
    if image.width() == 0 || image.height() == 0 {
        return Err("BiRefNet image dimensions must be non-zero".into());
    }
    let model = GgufModel::open(model)?;
    match backend {
        Backend::Cpu => birefnet_inner::<burn::backend::NdArray>(&model, image),
        Backend::Gpu => birefnet_inner::<burn::backend::Wgpu>(&model, image),
    }
}

/// Backend-generic graph entry point.  Weight tensors are intentionally read
/// through `GgufModel`, never by unchecked offset arithmetic.
fn birefnet_inner<B: BurnBackend>(
    model: &GgufModel,
    image: &RgbaImage,
) -> Result<GrayImage, String> {
    let image_size = u32::try_from(model.metadata_i64("birefnet.image_size")?)
        .map_err(|_| "BiRefNet image size is invalid")?;
    let multiple = u32::try_from(model.metadata_i64("birefnet.image_multiple")?)
        .map_err(|_| "BiRefNet image multiple is invalid")?;
    if image_size == 0 || multiple == 0 || image_size % multiple != 0 {
        return Err("BiRefNet image metadata is invalid".into());
    }
    if image_size != 1024 {
        return Err(format!(
            "this BiRefNet Burn graph supports only the checked 1024-pixel export, not image_size={image_size}"
        ));
    }
    if model.metadata_i64("swin.embed_dim")? != 192 {
        return Err(
            "this BiRefNet Burn graph currently supports only the 192-dimension Swin-L export"
                .into(),
        );
    }
    let resized = (image.dimensions() != (image_size, image_size))
        .then(|| resample::resize_rgba(image, image_size, image_size));
    let input_image = resized.as_ref().unwrap_or(image);
    let weights = Model::<B>::new(model)?;
    let input = weights.input(input_image)?;
    let prediction = weights.predict(input)?;
    let shape = prediction.dims();
    if shape != [1, 1, image_size as usize, image_size as usize] {
        return Err(format!(
            "BiRefNet returned unexpected tensor shape {shape:?}"
        ));
    }
    let alpha = prediction
        .into_data()
        .to_vec::<f32>()
        .map_err(|error| format!("cannot read BiRefNet output: {error}"))?;
    if alpha.len() != image_size as usize * image_size as usize
        || !alpha.iter().all(|value| value.is_finite())
    {
        return Err("BiRefNet returned non-finite or incorrectly sized alpha".into());
    }
    Ok(resample::resize_mask(
        &alpha,
        image_size,
        image_size,
        image.width(),
        image.height(),
    ))
}

/// A checked GGUF view bound to one Burn device. GGUF dimensions are native
/// reverse order, while Burn tensors are conventional row-major NCHW/OIHW.
struct Model<'a, B: BurnBackend> {
    gguf: &'a GgufModel,
    device: B::Device,
    conv_weights_are_nhwc: bool,
}

impl<'a, B: BurnBackend> Model<'a, B> {
    fn new(gguf: &'a GgufModel) -> Result<Self, String> {
        let _ = gguf.named_tensor("bb.patch_embed.proj.weight")?;
        let conv_weights_are_nhwc = match gguf.metadata_string("birefnet.tensor_data_layout")? {
            "cwhn" => true,
            "whcn" => false,
            layout => {
                return Err(format!(
                    "unsupported BiRefNet convolution layout {layout:?}"
                ));
            }
        };
        Ok(Self {
            gguf,
            device: B::Device::default(),
            conv_weights_are_nhwc,
        })
    }

    fn input(&self, image: &RgbaImage) -> Result<Tensor<B, 4>, String> {
        let (width, height) = image.dimensions();
        let mut values = vec![0.0; 3 * width as usize * height as usize];
        for (x, y, pixel) in image.enumerate_pixels() {
            let index = y as usize * width as usize + x as usize;
            // This path deliberately has no resize: caller checked 1024².
            values[index] = (f32::from(pixel[0]) / 255.0 - 0.485) / 0.229;
            values[width as usize * height as usize + index] =
                (f32::from(pixel[1]) / 255.0 - 0.456) / 0.224;
            values[2 * width as usize * height as usize + index] =
                (f32::from(pixel[2]) / 255.0 - 0.406) / 0.225;
        }
        Ok(Tensor::from_data(
            TensorData::new(values, Shape::new([1, 3, height as usize, width as usize])),
            &self.device,
        ))
    }

    fn tensor<const D: usize>(
        &self,
        name: &str,
        shape: [usize; D],
    ) -> Result<Tensor<B, D>, String> {
        let info = self.gguf.named_tensor(name)?;
        let expected = shape
            .iter()
            .try_fold(1_u64, |elements, dimension| {
                elements.checked_mul(*dimension as u64)
            })
            .ok_or("Burn tensor shape overflows")?;
        if info.byte_len
            != expected
                .checked_mul(if info.dtype == 1 { 2 } else { 4 })
                .ok_or("Burn tensor byte size overflows")?
        {
            return Err(format!(
                "GGUF tensor {name:?} is incompatible with Burn shape {shape:?}"
            ));
        }
        Ok(Tensor::from_data(
            TensorData::new(self.gguf.tensor_f32(info)?, Shape::new(shape)),
            &self.device,
        ))
    }

    fn conv(
        &self,
        name: &str,
        x: Tensor<B, 4>,
        stride: usize,
        padding: usize,
    ) -> Result<Tensor<B, 4>, String> {
        let weight_info = self.gguf.named_tensor(&format!("{name}.weight"))?;
        if weight_info.dimensions.len() != 4 {
            return Err(format!("convolution {name:?} does not have rank four"));
        }
        let shape = [
            weight_info.dimensions[3] as usize,
            weight_info.dimensions[2] as usize,
            weight_info.dimensions[1] as usize,
            weight_info.dimensions[0] as usize,
        ];
        let weight = if self.conv_weights_are_nhwc {
            self.conv_weight_nhwc(&format!("{name}.weight"), weight_info)?
        } else {
            self.tensor(&format!("{name}.weight"), shape)?
        };
        let bias = self
            .gguf
            .named_tensor(&format!("{name}.bias"))
            .ok()
            .map(|info| self.tensor(&info.name, [shape[0]]))
            .transpose()?;
        Ok(conv2d(
            x,
            weight,
            bias,
            ConvOptions::new([stride, stride], [padding, padding], [1, 1], 1),
        ))
    }

    fn conv_weight_nhwc(
        &self,
        name: &str,
        info: &crate::gguf::TensorInfo,
    ) -> Result<Tensor<B, 4>, String> {
        let raw = self.gguf.tensor_f32(info)?;
        let (oihw, [output_channels, input_channels, kernel_h, kernel_w]) =
            ohwi_to_oihw(name, &info.dimensions, raw)?;
        Ok(Tensor::from_data(
            TensorData::new(
                oihw,
                Shape::new([output_channels, input_channels, kernel_h, kernel_w]),
            ),
            &self.device,
        ))
    }

    fn predict(&self, input: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        let (patch, patch_bias) = self.patch_kernel()?;
        let original = input.clone();
        let x = conv2d(
            input,
            patch.clone(),
            Some(patch_bias.clone()),
            ConvOptions::new([4, 4], [0, 0], [1, 1], 1),
        );
        let main = self.swin_encoder(x)?;
        let low_input = bilinear_resize(
            contiguous_nchw(original.clone()),
            [original.dims()[2] / 2, original.dims()[3] / 2],
        );
        let low_patch = conv2d(
            low_input,
            patch,
            Some(patch_bias),
            ConvOptions::new([4, 4], [0, 0], [1, 1], 1),
        );
        let low = self.swin_encoder(low_patch)?;
        let features = self.concat_encoder_scales(main, low)?;
        self.decode(features, original)
    }

    fn patch_kernel(&self) -> Result<(Tensor<B, 4>, Tensor<B, 1>), String> {
        // The patch embedding is the single converter exception: GGUF native
        // [I, W, H, O] holds a Python OHWI kernel; Burn requires OIHW.
        let info = self.gguf.named_tensor("bb.patch_embed.proj.weight")?;
        let patch = self.conv_weight_nhwc("bb.patch_embed.proj.weight", info)?;
        let patch_bias = self.tensor("bb.patch_embed.proj.bias", [patch.dims()[0]])?;
        Ok((patch, patch_bias))
    }

    fn swin_encoder(&self, x: Tensor<B, 4>) -> Result<Vec<Tensor<B, 4>>, String> {
        // Burn convolution is NCHW; Swin operates NHWC so LayerNorm and Linear
        // are applied over the final feature axis, matching vision.cpp.
        let mut x = self.layer_norm_last("bb.patch_embed.norm", x.permute([0, 2, 3, 1]))?;
        let layouts = [(2, 6), (2, 12), (18, 24), (2, 48)];
        let mut outputs = Vec::with_capacity(4);
        for (layer, (depth, heads)) in layouts.into_iter().enumerate() {
            for block in 0..depth {
                x = self.swin_block(
                    &format!("bb.layers.{layer}.blocks.{block}"),
                    x,
                    heads,
                    12,
                    if block % 2 == 0 { 0 } else { 6 },
                )?;
            }
            outputs.push(
                self.layer_norm_last(&format!("bb.norm{layer}"), x.clone())?
                    .permute([0, 3, 1, 2]),
            );
            if layer < 3 {
                x = self.patch_merge(&format!("bb.layers.{layer}.downsample"), x)?;
            }
        }
        Ok(outputs)
    }

    fn patch_merge(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        let [_, height, width, _] = x.dims();
        if height % 2 != 0 || width % 2 != 0 {
            return Err("Swin patch merging requires even dimensions".into());
        }
        let even_y = self.indices(0, height, 2);
        let odd_y = self.indices(1, height, 2);
        let even_x = self.indices(0, width, 2);
        let odd_x = self.indices(1, width, 2);
        // vision.cpp order: TL, BL, TR, BR.
        let tl = x
            .clone()
            .select(1, even_y.clone())
            .select(2, even_x.clone());
        let bl = x.clone().select(1, odd_y.clone()).select(2, even_x);
        let tr = x.clone().select(1, even_y).select(2, odd_x.clone());
        let br = x.select(1, odd_y).select(2, odd_x);
        let merged = Tensor::cat(vec![tl, bl, tr, br], 3);
        let merged = self.layer_norm_last(&format!("{name}.norm"), merged)?;
        self.linear(&format!("{name}.reduction"), merged)
    }

    fn indices(&self, start: usize, end: usize, step: usize) -> Tensor<B, 1, Int> {
        Tensor::from_data(
            TensorData::new(
                (start..end)
                    .step_by(step)
                    .map(|value| value as i32)
                    .collect::<Vec<_>>(),
                Shape::new([(end - start).div_ceil(step)]),
            ),
            &self.device,
        )
    }

    fn concat_encoder_scales(
        &self,
        mut main: Vec<Tensor<B, 4>>,
        low: Vec<Tensor<B, 4>>,
    ) -> Result<Vec<Tensor<B, 4>>, String> {
        if main.len() != 4 || low.len() != 4 {
            return Err("BiRefNet encoder did not produce four feature maps".into());
        }
        for index in 0..4 {
            let [_, _, height, width] = main[index].dims();
            main[index] = Tensor::cat(
                vec![
                    main[index].clone(),
                    bilinear_resize(low[index].clone(), [height, width]),
                ],
                1,
            );
        }
        let target = main[3].dims();
        let scaled = [
            bilinear_resize(main[0].clone(), [target[2], target[3]]),
            bilinear_resize(main[1].clone(), [target[2], target[3]]),
            bilinear_resize(main[2].clone(), [target[2], target[3]]),
            main[3].clone(),
        ];
        main[3] = Tensor::cat(Vec::from(scaled), 1);
        Ok(main)
    }

    fn decode(
        &self,
        features: Vec<Tensor<B, 4>>,
        image: Tensor<B, 4>,
    ) -> Result<Tensor<B, 4>, String> {
        let [x1, x2, x3, x4]: [Tensor<B, 4>; 4] = features
            .try_into()
            .map_err(|_| "BiRefNet decoder needs four feature maps")?;
        let x4 = self.basic_decoder("squeeze_module.0", x4)?;
        let x4_height = x4.dims()[2];
        let x4_width = x4.dims()[3];
        let p4 = self.basic_decoder(
            "decoder.block4",
            Tensor::cat(
                vec![
                    x4,
                    self.simple_conv(
                        "decoder.ipt_blk5",
                        self.image_to_patches(image.clone(), x4_height, x4_width)?,
                    )?,
                ],
                1,
            ),
        )?;
        let p4 = self.gated("decoder.gdt_convs_4", "decoder.gdt_convs_attn_4.0", p4)?;
        let p3_input = bilinear_resize(p4, [x3.dims()[2], x3.dims()[3]]).add(self.conv(
            "decoder.lateral_block4.conv",
            x3,
            1,
            0,
        )?);
        let p3_height = p3_input.dims()[2];
        let p3_width = p3_input.dims()[3];
        let p3 = self.basic_decoder(
            "decoder.block3",
            Tensor::cat(
                vec![
                    p3_input,
                    self.simple_conv(
                        "decoder.ipt_blk4",
                        self.image_to_patches(image.clone(), p3_height, p3_width)?,
                    )?,
                ],
                1,
            ),
        )?;
        let p3 = self.gated("decoder.gdt_convs_3", "decoder.gdt_convs_attn_3.0", p3)?;
        let p2_input = bilinear_resize(p3, [x2.dims()[2], x2.dims()[3]]).add(self.conv(
            "decoder.lateral_block3.conv",
            x2,
            1,
            0,
        )?);
        let p2_height = p2_input.dims()[2];
        let p2_width = p2_input.dims()[3];
        let p2 = self.basic_decoder(
            "decoder.block2",
            Tensor::cat(
                vec![
                    p2_input,
                    self.simple_conv(
                        "decoder.ipt_blk3",
                        self.image_to_patches(image.clone(), p2_height, p2_width)?,
                    )?,
                ],
                1,
            ),
        )?;
        let p2 = self.gated("decoder.gdt_convs_2", "decoder.gdt_convs_attn_2.0", p2)?;
        let p1 = bilinear_resize(p2, [x1.dims()[2], x1.dims()[3]]).add(self.conv(
            "decoder.lateral_block2.conv",
            x1,
            1,
            0,
        )?);
        let p1_height = p1.dims()[2];
        let p1_width = p1.dims()[3];
        let p1 = self.basic_decoder(
            "decoder.block1",
            Tensor::cat(
                vec![
                    p1,
                    self.simple_conv(
                        "decoder.ipt_blk2",
                        self.image_to_patches(image.clone(), p1_height, p1_width)?,
                    )?,
                ],
                1,
            ),
        )?;
        let p1 = bilinear_resize(p1, [image.dims()[2], image.dims()[3]]);
        let p1 = Tensor::cat(vec![p1, self.simple_conv("decoder.ipt_blk1", image)?], 1);
        Ok(sigmoid(self.conv("decoder.conv_out1.0", p1, 1, 0)?))
    }

    fn image_to_patches(
        &self,
        image: Tensor<B, 4>,
        output_height: usize,
        output_width: usize,
    ) -> Result<Tensor<B, 4>, String> {
        image_to_patches(image, output_height, output_width)
    }

    fn batch_norm(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        let channels = x.dims()[1];
        let weight = self.tensor(&format!("{name}.weight"), [1, channels, 1, 1])?;
        let bias = self.tensor(&format!("{name}.bias"), [1, channels, 1, 1])?;
        Ok(x.mul(weight).add(bias))
    }

    fn deformable_conv(
        &self,
        name: &str,
        x: Tensor<B, 4>,
        padding: usize,
    ) -> Result<Tensor<B, 4>, String> {
        let offset = self.conv(&format!("{name}.offset"), x.clone(), 1, padding)?;
        let modulator = sigmoid(self.conv(&format!("{name}.modulator"), x.clone(), 1, padding)?)
            .mul_scalar(2.0);
        let info = self.gguf.named_tensor(&format!("{name}.conv.weight"))?;
        let shape = [
            info.dimensions[3] as usize,
            info.dimensions[2] as usize,
            info.dimensions[1] as usize,
            info.dimensions[0] as usize,
        ];
        let weight_name = format!("{name}.conv.weight");
        let weight = if self.conv_weights_are_nhwc {
            self.conv_weight_nhwc(&weight_name, info)?
        } else {
            self.tensor(&weight_name, shape)?
        };
        Ok(deform_conv2d(
            x,
            offset,
            weight,
            Some(modulator),
            None,
            DeformConvOptions::new([1, 1], [padding, padding], [1, 1], 1, 1),
        ))
    }

    fn aspp_module(
        &self,
        name: &str,
        x: Tensor<B, 4>,
        padding: usize,
    ) -> Result<Tensor<B, 4>, String> {
        Ok(relu(self.batch_norm(
            &format!("{name}.bn"),
            self.deformable_conv(&format!("{name}.conv"), x, padding)?,
        )?))
    }

    fn aspp(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        let x1 = self.aspp_module(&format!("{name}.aspp1"), x.clone(), 0)?;
        let x2 = self.aspp_module(&format!("{name}.aspp_deforms.0"), x.clone(), 0)?;
        let x3 = self.aspp_module(&format!("{name}.aspp_deforms.1"), x.clone(), 1)?;
        let x4 = self.aspp_module(&format!("{name}.aspp_deforms.2"), x.clone(), 3)?;
        let pooled = relu(self.conv(
            &format!("{name}.global_avg_pool.1"),
            adaptive_avg_pool2d(x, [1, 1]),
            1,
            0,
        )?);
        let x5 = bilinear_resize(pooled, [x1.dims()[2], x1.dims()[3]]);
        Ok(relu(self.conv(
            &format!("{name}.conv1"),
            Tensor::cat(vec![x1, x2, x3, x4, x5], 1),
            1,
            0,
        )?))
    }

    fn basic_decoder(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        let x = relu(self.conv(&format!("{name}.conv_in"), x, 1, 1)?);
        let x = self.aspp(&format!("{name}.dec_att"), x)?;
        self.conv(&format!("{name}.conv_out"), x, 1, 1)
    }

    fn simple_conv(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        self.conv(
            &format!("{name}.conv_out"),
            self.conv(&format!("{name}.conv1"), x, 1, 1)?,
            1,
            1,
        )
    }

    fn gdt(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        Ok(relu(self.conv(&format!("{name}.0"), x, 1, 1)?))
    }

    fn gated(
        &self,
        gdt_name: &str,
        attention_name: &str,
        x: Tensor<B, 4>,
    ) -> Result<Tensor<B, 4>, String> {
        let attention = sigmoid(self.conv(attention_name, self.gdt(gdt_name, x.clone())?, 1, 0)?);
        Ok(x.mul(attention))
    }

    fn layer_norm_last(&self, name: &str, x: Tensor<B, 4>) -> Result<Tensor<B, 4>, String> {
        let channels = x.dims()[3];
        let weight = self.tensor(&format!("{name}.weight"), [1, 1, 1, channels])?;
        let bias = self.tensor(&format!("{name}.bias"), [1, 1, 1, channels])?;
        let mean = x.clone().mean_dim(3);
        let variance = x.clone().sub(mean.clone()).powf_scalar(2.0).mean_dim(3);
        Ok(x.sub(mean)
            .div(variance.add_scalar(LAYER_NORM_EPSILON).sqrt())
            .mul(weight)
            .add(bias))
    }

    fn linear<const D: usize>(&self, name: &str, x: Tensor<B, D>) -> Result<Tensor<B, D>, String> {
        let info = self.gguf.named_tensor(&format!("{name}.weight"))?;
        if info.dimensions.len() != 2 {
            return Err(format!("linear {name:?} does not have rank two"));
        }
        let (inputs, outputs) = (info.dimensions[0] as usize, info.dimensions[1] as usize);
        let raw = self.gguf.tensor_f32(info)?;
        // PyTorch bytes are [out, in]; Burn's functional linear needs [in, out].
        let mut transposed = vec![0.0; raw.len()];
        for output in 0..outputs {
            for input in 0..inputs {
                transposed[input * outputs + output] = raw[output * inputs + input];
            }
        }
        let weight = Tensor::from_data(
            TensorData::new(transposed, Shape::new([inputs, outputs])),
            &self.device,
        );
        let bias = self
            .gguf
            .named_tensor(&format!("{name}.bias"))
            .ok()
            .map(|info| self.tensor(&info.name, [outputs]))
            .transpose()?;
        Ok(linear(x, weight, bias))
    }

    fn swin_block(
        &self,
        name: &str,
        x: Tensor<B, 4>,
        heads: usize,
        window: usize,
        shift: usize,
    ) -> Result<Tensor<B, 4>, String> {
        let shortcut = x.clone();
        let x = self.window_attention(
            &format!("{name}.attn"),
            self.layer_norm_last(&format!("{name}.norm1"), x)?,
            heads,
            window,
            shift,
        )?;
        let x = x.add(shortcut);
        let shortcut = x.clone();
        let x = gelu(self.linear(
            &format!("{name}.mlp.fc1"),
            self.layer_norm_last(&format!("{name}.norm2"), x)?,
        )?);
        Ok(self.linear(&format!("{name}.mlp.fc2"), x)?.add(shortcut))
    }

    fn window_attention(
        &self,
        name: &str,
        x: Tensor<B, 4>,
        heads: usize,
        window: usize,
        shift: usize,
    ) -> Result<Tensor<B, 4>, String> {
        let [batch, height, width, channels] = x.dims();
        if channels % heads != 0 {
            return Err(format!(
                "Swin channels {channels} are not divisible by heads {heads}"
            ));
        }
        let pad_right = (window - width % window) % window;
        let pad_bottom = (window - height % window) % window;
        // Burn pads its final dimensions, so move H/W to the end temporarily.
        let mut x = x
            .permute([0, 3, 1, 2])
            .pad((0, pad_right, 0, pad_bottom), PadMode::Constant(0.0))
            .permute([0, 2, 3, 1]);
        let padded_height = height + pad_bottom;
        let padded_width = width + pad_right;
        if shift > 0 {
            // Burn's `roll(+n)` reads from index+n, the opposite sign of
            // ggml_roll: C++ `roll(-shift)` is therefore Burn `roll(+shift)`.
            x = roll_spatial(x, shift, shift);
        }
        let windows_y = padded_height / window;
        let windows_x = padded_width / window;
        let patches = window * window;
        let window_count = batch * windows_y * windows_x;
        let partition = x
            .reshape([batch, windows_y, window, windows_x, window, channels])
            .permute([0, 1, 3, 2, 4, 5])
            .reshape([window_count, patches, channels]);
        let qkv = self.linear(&format!("{name}.qkv"), partition)?;
        let head_dim = channels / heads;
        let qkv = qkv
            .reshape([window_count, patches, 3, heads, head_dim])
            .permute([2, 0, 3, 1, 4]);
        let q = qkv
            .clone()
            .slice([0..1, 0..window_count, 0..heads, 0..patches, 0..head_dim])
            .squeeze_dim(0);
        let k = qkv
            .clone()
            .slice([1..2, 0..window_count, 0..heads, 0..patches, 0..head_dim])
            .squeeze_dim(0);
        let v = qkv
            .slice([2..3, 0..window_count, 0..heads, 0..patches, 0..head_dim])
            .squeeze_dim(0);
        let scale = (head_dim as f32).sqrt().recip();
        let scores = q.mul_scalar(scale).matmul(k.permute([0, 1, 3, 2]));
        let bias = self.relative_bias(name, heads, window)?;
        // Current vision.cpp applies the generated shifted-window mask to every
        // block, including the shift==0 blocks.
        let mask = self.attention_mask(padded_width, padded_height, window, window_count)?;
        let scores = scores.add(bias).add(mask);
        let attended = softmax(scores, 3).matmul(v).permute([0, 2, 1, 3]).reshape([
            window_count,
            patches,
            channels,
        ]);
        let output = self.linear(&format!("{name}.proj"), attended)?;
        let mut output = output
            .reshape([batch, windows_y, windows_x, window, window, channels])
            .permute([0, 1, 3, 2, 4, 5])
            .reshape([batch, padded_height, padded_width, channels]);
        if shift > 0 {
            output = roll_spatial(output, padded_height - shift, padded_width - shift);
        }
        Ok(output.slice([0..batch, 0..height, 0..width, 0..channels]))
    }

    fn relative_bias(
        &self,
        name: &str,
        heads: usize,
        window: usize,
    ) -> Result<Tensor<B, 4>, String> {
        let raw = self.gguf.tensor_f32(
            self.gguf
                .named_tensor(&format!("{name}.relative_position_bias_table"))?,
        )?;
        let rows = (2 * window - 1) * (2 * window - 1);
        if raw.len() != rows * heads {
            return Err(format!("relative-position bias {name:?} shape is invalid"));
        }
        let patches = window * window;
        let index = relative_position_indices(window);
        let mut values = vec![0.0; heads * patches * patches];
        for head in 0..heads {
            for query in 0..patches {
                for key in 0..patches {
                    values[(head * patches + query) * patches + key] =
                        raw[index[query * patches + key] as usize * heads + head];
                }
            }
        }
        Ok(Tensor::from_data(
            TensorData::new(values, Shape::new([1, heads, patches, patches])),
            &self.device,
        ))
    }

    fn attention_mask(
        &self,
        width: usize,
        height: usize,
        window: usize,
        window_count: usize,
    ) -> Result<Tensor<B, 4>, String> {
        let patches = window * window;
        let one_image = shifted_window_mask(width, height, window);
        if !window_count.is_multiple_of(width / window * (height / window)) {
            return Err("attention mask batch shape is invalid".into());
        }
        let windows_per_image = (width / window) * (height / window);
        let copies = window_count / windows_per_image;
        let mut values = Vec::with_capacity(window_count * patches * patches);
        for _ in 0..copies {
            values.extend_from_slice(&one_image);
        }
        Ok(Tensor::from_data(
            TensorData::new(values, Shape::new([window_count, 1, patches, patches])),
            &self.device,
        ))
    }
}

/// Converts an exported Python OHWI buffer to Burn's OIHW storage. GGUF
/// records native-reversed OHWI dimensions as `[I, W, H, O]`.
fn ohwi_to_oihw(
    name: &str,
    dimensions: &[u64],
    raw: Vec<f32>,
) -> Result<(Vec<f32>, [usize; 4]), String> {
    if dimensions.len() != 4 {
        return Err(format!("NHWC convolution {name:?} does not have rank four"));
    }
    let input_channels = usize::try_from(dimensions[0])
        .map_err(|_| format!("NHWC convolution {name:?} input dimension overflows usize"))?;
    let kernel_w = usize::try_from(dimensions[1])
        .map_err(|_| format!("NHWC convolution {name:?} width dimension overflows usize"))?;
    let kernel_h = usize::try_from(dimensions[2])
        .map_err(|_| format!("NHWC convolution {name:?} height dimension overflows usize"))?;
    let output_channels = usize::try_from(dimensions[3])
        .map_err(|_| format!("NHWC convolution {name:?} output dimension overflows usize"))?;
    let elements = input_channels
        .checked_mul(kernel_w)
        .and_then(|n| n.checked_mul(kernel_h))
        .and_then(|n| n.checked_mul(output_channels))
        .ok_or_else(|| format!("NHWC convolution {name:?} element count overflows usize"))?;
    if raw.len() != elements {
        return Err(format!(
            "NHWC convolution {name:?} has {} values, expected {elements}",
            raw.len()
        ));
    }
    let mut oihw = vec![0.0; elements];
    for output in 0..output_channels {
        for y in 0..kernel_h {
            for x in 0..kernel_w {
                for input in 0..input_channels {
                    let source =
                        (((output * kernel_h + y) * kernel_w + x) * input_channels) + input;
                    let destination =
                        (((output * input_channels + input) * kernel_h + y) * kernel_w) + x;
                    oihw[destination] = raw[source];
                }
            }
        }
    }
    Ok((oihw, [output_channels, input_channels, kernel_h, kernel_w]))
}

/// Burn's WGPU bilinear kernel requires physical NCHW-contiguous input. Swin
/// features are NHWC buffers viewed through a permutation, so reshape through
/// a different rank to materialize their logical NCHW order before resizing.
fn contiguous_nchw<B: BurnBackend>(image: Tensor<B, 4>) -> Tensor<B, 4> {
    let [batch, channels, height, width] = image.dims();
    image
        .reshape([batch * channels * height * width])
        .reshape([batch, channels, height, width])
}

/// BiRefNet's reference uses align-corners bilinear resize. Burn's WGPU
/// kernel requires scalar-line contiguous NCHW storage after Swin's NHWC
/// views. A 1x1 source has an exact constant result, avoiding that kernel.
fn bilinear_resize<B: BurnBackend>(image: Tensor<B, 4>, output: [usize; 2]) -> Tensor<B, 4> {
    let [_, _, height, width] = image.dims();
    if height == 1 && width == 1 {
        return image.repeat_dim(2, output[0]).repeat_dim(3, output[1]);
    }
    interpolate(
        contiguous_nchw(image),
        output,
        InterpolateOptions::new(InterpolateMode::Bilinear),
    )
}

/// C++ `image_to_patches` in NCHW form. The free helper is shared by the
/// decoder and regression test, so its reshape ordering cannot diverge.
fn image_to_patches<B: BurnBackend>(
    image: Tensor<B, 4>,
    output_height: usize,
    output_width: usize,
) -> Result<Tensor<B, 4>, String> {
    let [batch, channels, height, width] = image.dims();
    if height % output_height != 0 || width % output_width != 0 {
        return Err("BiRefNet image-to-patches extent does not divide the image".into());
    }
    let grid_h = height / output_height;
    let grid_w = width / output_width;
    Ok(image
        .reshape([batch, channels, grid_h, output_height, grid_w, output_width])
        .permute([0, 1, 2, 4, 3, 5])
        .reshape([
            batch,
            channels * grid_h * grid_w,
            output_height,
            output_width,
        ]))
}

fn roll_spatial<B: BurnBackend>(
    image: Tensor<B, 4>,
    vertical: usize,
    horizontal: usize,
) -> Tensor<B, 4> {
    image.roll(&[vertical, horizontal], &[1, 2])
}

/// Matches `swin::compute_relative_position_index` exactly.  The result is
/// indexed by query-patch then key-patch in row-major window order.
pub(crate) fn relative_position_indices(window: usize) -> Vec<i32> {
    let patches = window * window;
    (0..patches * patches)
        .map(|index| {
            let x0 = index % window;
            let y0 = (index / window) % window;
            let x1 = (index / patches) % window;
            let y1 = (index / patches / window) % window;
            ((y1 as i32 - y0 as i32 + window as i32 - 1) * (2 * window as i32 - 1)) + x1 as i32
                - x0 as i32
                + window as i32
                - 1
        })
        .collect()
}

/// The current vision.cpp graph supplies this mask to *every* Swin block,
/// including unshifted blocks.  Edge windows are zero except for pairs split
/// by the shifted-window boundary, which are negative infinity.
pub(crate) fn shifted_window_mask(width: usize, height: usize, window: usize) -> Vec<f32> {
    assert!(window > 0);
    let patches = window * window;
    let windows_x = width.div_ceil(window);
    let windows_y = height.div_ceil(window);
    let padded_width = windows_x * window;
    let padded_height = windows_y * window;
    let shift = window / 2;
    let mut output = vec![0.0; windows_x * windows_y * patches * patches];
    for window_y in 0..windows_y {
        for window_x in 0..windows_x {
            if window_y + 1 < windows_y && window_x + 1 < windows_x {
                continue;
            }
            let base = (window_y * windows_x + window_x) * patches * patches;
            for y0 in 0..window {
                for x0 in 0..window {
                    for y1 in 0..window {
                        for x1 in 0..window {
                            let same_y = (window_y * window + y0 < padded_height - shift)
                                == (window_y * window + y1 < padded_height - shift);
                            let same_x = (window_x * window + x0 < padded_width - shift)
                                == (window_x * window + x1 < padded_width - shift);
                            if !same_y || !same_x {
                                output[base + (y0 * window + x0) * patches + y1 * window + x1] =
                                    f32::NEG_INFINITY;
                            }
                        }
                    }
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn::backend::NdArray;
    use std::time::Instant;

    #[test]
    fn relative_position_order_matches_three_by_three_reference() {
        let index = relative_position_indices(3);
        assert_eq!(index.len(), 81);
        assert_eq!(&index[..9], &[12, 11, 10, 7, 6, 5, 2, 1, 0]);
        assert_eq!(index[40], 12);
    }

    #[test]
    fn shifted_window_masks_edges_but_not_interior_windows() {
        let mask = shifted_window_mask(6, 6, 3);
        assert!(mask[..81].iter().all(|value| *value == 0.0));
        assert!(
            mask[81..]
                .iter()
                .any(|value| value.is_infinite() && value.is_sign_negative())
        );
    }

    #[test]
    fn ohwi_kernel_conversion_preserves_each_asymmetric_coordinate() {
        // Native GGUF dimensions reverse Python [O, H, W, I] to [I, W, H, O].
        // Each value encodes all four Python coordinates, so a swapped H/W or
        // input/output axis cannot pass this regression test.
        let mut raw = Vec::new();
        for output in 0..2 {
            for y in 0..2 {
                for x in 0..3 {
                    for input in 0..2 {
                        raw.push((1000 * output + 100 * y + 10 * x + input) as f32);
                    }
                }
            }
        }
        let (oihw, shape) = ohwi_to_oihw("asymmetric", &[2, 3, 2, 2], raw).unwrap();
        assert_eq!(shape, [2, 2, 2, 3]);
        for output in 0..2 {
            for input in 0..2 {
                for y in 0..2 {
                    for x in 0..3 {
                        let offset = (((output * 2 + input) * 2 + y) * 3) + x;
                        assert_eq!(
                            oihw[offset],
                            (1000 * output + 100 * y + 10 * x + input) as f32
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn image_to_patches_uses_contiguous_quadrants() {
        let device = <NdArray as BurnBackend>::Device::default();
        let image = Tensor::<NdArray, 4>::from_data(
            TensorData::new(
                (0..16).map(|value| value as f32).collect::<Vec<_>>(),
                Shape::new([1, 1, 4, 4]),
            ),
            &device,
        );
        let data = image_to_patches(image, 2, 2)
            .unwrap()
            .into_data()
            .to_vec::<f32>()
            .unwrap();
        assert_eq!(
            data,
            vec![
                0., 1., 4., 5., 2., 3., 6., 7., 8., 9., 12., 13., 10., 11., 14., 15.
            ]
        );
    }

    #[test]
    fn inverse_shift_restores_a_tensor() {
        let device = Default::default();
        let source = Tensor::<NdArray, 4>::from_data(
            TensorData::new(
                (0..24 * 24).map(|value| value as f32).collect::<Vec<_>>(),
                Shape::new([1, 24, 24, 1]),
            ),
            &device,
        );
        // Burn's roll direction is inverse to ggml_roll. At side 24, C++
        // roll(-6) is Burn roll(+6), and its inverse is Burn roll(+18).
        let shifted = roll_spatial(source.clone(), 6, 6);
        // The top-left output must receive source[y=6, x=6].
        assert_eq!(
            shifted.clone().into_data().to_vec::<f32>().unwrap()[0],
            150.0
        );
        let round_trip = roll_spatial(shifted, 18, 18);
        assert_eq!(
            round_trip.into_data().to_vec::<f32>().unwrap(),
            source.into_data().to_vec::<f32>().unwrap()
        );
    }

    #[test]
    #[ignore = "exercises WGPU resize on materialized Swin views"]
    fn wgpu_materialized_encoder_scale_resize_matches_cpu() {
        let cpu = Tensor::<NdArray, 4>::from_data(
            TensorData::new(
                (0..32 * 32 * 384)
                    .map(|value| (value % 97) as f32 * 0.03125 - 1.0)
                    .collect::<Vec<_>>(),
                Shape::new([1, 32, 32, 384]),
            ),
            &Default::default(),
        )
        .permute([0, 3, 1, 2]);
        let gpu = Tensor::<burn::backend::Wgpu, 4>::from_data(
            TensorData::new(
                (0..32 * 32 * 384)
                    .map(|value| (value % 97) as f32 * 0.03125 - 1.0)
                    .collect::<Vec<_>>(),
                Shape::new([1, 32, 32, 384]),
            ),
            &Default::default(),
        )
        .permute([0, 3, 1, 2]);
        let cpu = bilinear_resize(cpu, [4, 4])
            .into_data()
            .to_vec::<f32>()
            .unwrap();
        let gpu = bilinear_resize(gpu, [4, 4])
            .into_data()
            .to_vec::<f32>()
            .unwrap();
        let mae = cpu
            .iter()
            .zip(&gpu)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / cpu.len() as f32;
        assert!(
            mae < 1e-4,
            "non-contiguous encoder resize CPU/WGPU MAE={mae}"
        );

        fn singleton<B: BurnBackend>() -> Vec<f32> {
            let device = B::Device::default();
            bilinear_resize(
                Tensor::<B, 4>::from_data(
                    TensorData::new(
                        (0..32).map(|value| value as f32 - 16.0).collect::<Vec<_>>(),
                        Shape::new([1, 32, 1, 1]),
                    ),
                    &device,
                ),
                [5, 7],
            )
            .into_data()
            .to_vec::<f32>()
            .unwrap()
        }
        assert_eq!(singleton::<NdArray>(), singleton::<burn::backend::Wgpu>());
    }

    #[test]
    #[ignore = "manual parity run against the checked developer GGUF"]
    fn reference_gpu_writes_soft_alpha() {
        let model = std::path::PathBuf::from(
            std::env::var_os("DIORAMA_BIREFNET_MODEL")
                .expect("set DIORAMA_BIREFNET_MODEL to run BiRefNet GPU parity"),
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/experiments/birefnet-parity");
        std::fs::create_dir_all(&root).expect("create parity output directory");
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fixtures = source_root.join("crates/diorama-inference/tests/fixtures/birefnet");
        for (input, output, reference, max_mae, min_iou) in [
            (
                fixtures.join("elf-1024.png"),
                root.join("native-gpu-alpha.png"),
                fixtures.join("vision-elf-alpha.png"),
                0.3,
                0.998,
            ),
            (
                fixtures.join("selection-1024.png"),
                root.join("native-gpu-selection-alpha.png"),
                fixtures.join("vision-selection-alpha.png"),
                0.3,
                0.998,
            ),
            (
                fixtures.join("selection-1280x800.png"),
                root.join("native-gpu-selection-original-alpha.png"),
                fixtures.join("vision-selection-original-alpha.png"),
                0.5,
                0.996,
            ),
        ] {
            let image = image::open(&input).expect("BiRefNet fixture").to_rgba8();
            let started = Instant::now();
            let alpha = birefnet(&model, &image, Backend::Gpu).expect("native GPU BiRefNet");
            alpha.save(&output).expect("write alpha");
            let reference = image::open(&reference).expect("reference alpha").to_luma8();
            assert_eq!(alpha.dimensions(), reference.dimensions());
            let mut absolute_error = 0_u64;
            let mut intersection = 0_u64;
            let mut union = 0_u64;
            for (actual, expected) in alpha.pixels().zip(reference.pixels()) {
                absolute_error += u64::from(actual[0].abs_diff(expected[0]));
                let actual_foreground = actual[0] >= 128;
                let expected_foreground = expected[0] >= 128;
                intersection += u64::from(actual_foreground && expected_foreground);
                union += u64::from(actual_foreground || expected_foreground);
            }
            let mae = absolute_error as f64 / alpha.len() as f64;
            let iou = intersection as f64 / union.max(1) as f64;
            assert!(mae < max_mae, "{} alpha MAE is {mae}", input.display());
            assert!(iou > min_iou, "{} alpha IoU is {iou}", input.display());
            eprintln!(
                "native GPU BiRefNet {} completed in {:?}; MAE={mae}, IoU={iou}",
                input.display(),
                started.elapsed()
            );
        }
    }

    #[test]
    #[ignore = "manual CPU fallback run against the checked developer GGUF"]
    fn reference_cpu_writes_soft_alpha() {
        let model = std::path::PathBuf::from(
            std::env::var_os("DIORAMA_BIREFNET_MODEL")
                .expect("set DIORAMA_BIREFNET_MODEL to run BiRefNet CPU parity"),
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/experiments/birefnet-parity");
        std::fs::create_dir_all(&root).expect("create parity output directory");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("crates/diorama-inference/tests/fixtures/birefnet");
        for (input, output, reference, max_mae, min_iou) in [
            (
                fixtures.join("elf-1024.png"),
                root.join("native-cpu-alpha.png"),
                fixtures.join("vision-elf-alpha.png"),
                0.3,
                0.998,
            ),
            (
                fixtures.join("selection-1280x800.png"),
                root.join("native-cpu-selection-original-alpha.png"),
                fixtures.join("vision-selection-original-alpha.png"),
                0.5,
                0.996,
            ),
        ] {
            let image = image::open(&input).expect("BiRefNet fixture").to_rgba8();
            let started = Instant::now();
            let alpha = birefnet(&model, &image, Backend::Cpu).expect("native CPU BiRefNet");
            alpha.save(&output).expect("write alpha");
            let reference = image::open(&reference).expect("reference alpha").to_luma8();
            assert_eq!(alpha.dimensions(), reference.dimensions());
            let mae = alpha
                .pixels()
                .zip(reference.pixels())
                .map(|(actual, expected)| f64::from(actual[0].abs_diff(expected[0])))
                .sum::<f64>()
                / alpha.len() as f64;
            let (mut intersection, mut union) = (0_u64, 0_u64);
            for (actual, expected) in alpha.pixels().zip(reference.pixels()) {
                let actual_foreground = actual[0] >= 128;
                let expected_foreground = expected[0] >= 128;
                intersection += u64::from(actual_foreground && expected_foreground);
                union += u64::from(actual_foreground || expected_foreground);
            }
            let iou = intersection as f64 / union.max(1) as f64;
            assert!(mae < max_mae, "{} CPU alpha MAE is {mae}", input.display());
            assert!(iou > min_iou, "{} CPU alpha IoU is {iou}", input.display());
            eprintln!(
                "native CPU BiRefNet {} completed in {:?}; MAE={mae}",
                input.display(),
                started.elapsed()
            );
        }
    }
}
