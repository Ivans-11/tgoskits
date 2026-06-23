#include "pose_model.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <algorithm>
#include <utility>

#include "file_utils.h"

namespace {

void dump_tensor_attr(const char *model_name, const char *kind, const rknn_tensor_attr *attr)
{
    printf("POSE_TENSOR model=%s kind=%s index=%d name=%s n_dims=%d dims=[%d,%d,%d,%d] n_elems=%d size=%d fmt=%s type=%s qnt_type=%s zp=%d scale=%f\n",
           model_name,
           kind,
           attr->index,
           attr->name,
           attr->n_dims,
           attr->dims[0],
           attr->dims[1],
           attr->dims[2],
           attr->dims[3],
           attr->n_elems,
           attr->size,
           get_format_string(attr->fmt),
           get_type_string(attr->type),
           get_qnt_type_string(attr->qnt_type),
           attr->zp,
           attr->scale);
}

double monotonic_ms()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec * 1000.0 + (double)ts.tv_nsec / 1000000.0;
}

void dump_output_stats(const char *model_name, int index, const rknn_tensor_attr *attr, const void *buf, size_t size)
{
    const unsigned char *data = (const unsigned char *)buf;
    uint64_t fnv = 1469598103934665603ULL;
    unsigned int umin = 255;
    unsigned int umax = 0;
    long long isum = 0;

    for (size_t i = 0; i < size; i++) {
        unsigned int value = data[i];
        umin = std::min(umin, value);
        umax = std::max(umax, value);
        isum += (signed char)data[i];
        fnv ^= value;
        fnv *= 1099511628211ULL;
    }

    printf("POSE_OUTPUT_STATS model=%s index=%d bytes=%u dims=[%d,%d,%d,%d] type=%s qnt_type=%s zp=%d scale=%f umin=%u umax=%u isum=%lld fnv=0x%llx first16=",
           model_name,
           index,
           (unsigned int)size,
           attr->dims[0],
           attr->dims[1],
           attr->dims[2],
           attr->dims[3],
           get_type_string(attr->type),
           get_qnt_type_string(attr->qnt_type),
           attr->zp,
           attr->scale,
           umin,
           umax,
           isum,
           (unsigned long long)fnv);
    size_t first = std::min<size_t>(size, 16);
    for (size_t i = 0; i < first; i++) {
        printf("%02x", data[i]);
    }
    printf("\n");
}

void infer_model_shape(pose_rknn_model_t *model)
{
    if (model == NULL || model->input_attrs.empty()) {
        return;
    }

    const rknn_tensor_attr &input = model->input_attrs[0];
    if (input.fmt == RKNN_TENSOR_NCHW) {
        model->model_channel = input.dims[1];
        model->model_height = input.dims[2];
        model->model_width = input.dims[3];
    } else {
        int channel_dim = -1;
        for (int i = 1; i < input.n_dims && i < 4; i++) {
            if (input.dims[i] == 3 || input.dims[i] == 1) {
                channel_dim = i;
                break;
            }
        }

        if (channel_dim > 0) {
            int spatial[2] = {0, 0};
            int spatial_count = 0;
            for (int i = 1; i < input.n_dims && i < 4; i++) {
                if (i == channel_dim) {
                    continue;
                }
                if (spatial_count < 2) {
                    spatial[spatial_count++] = input.dims[i];
                }
            }
            if (spatial_count == 2) {
                model->model_height = spatial[0];
                model->model_width = spatial[1];
                model->model_channel = input.dims[channel_dim];
                return;
            }
        }

        model->model_height = input.dims[1];
        model->model_width = input.dims[2];
        model->model_channel = input.dims[3];
    }
}

}  // namespace

int init_pose_rknn_model(const char *model_path, const char *name, pose_rknn_model_t *model)
{
    if (model_path == NULL || model_path[0] == '\0' || model == NULL) {
        return -1;
    }

    release_pose_rknn_model(model);
    model->name = (name != NULL && name[0] != '\0') ? name : "pose";

    int model_len = 0;
    char *model_data = NULL;
    model_len = read_data_from_file(model_path, &model_data);
    if (model_data == NULL || model_len <= 0) {
        printf("POSE_MODEL_LOAD_FAIL model=%s path=%s\n", model->name.c_str(), model_path);
        return -1;
    }

    int ret = rknn_init(&model->ctx, model_data, model_len, 0, NULL);
    free(model_data);
    if (ret < 0) {
        printf("POSE_MODEL_INIT_FAIL model=%s path=%s ret=%d\n", model->name.c_str(), model_path, ret);
        return -1;
    }

    memset(&model->io_num, 0, sizeof(model->io_num));
    ret = rknn_query(model->ctx, RKNN_QUERY_IN_OUT_NUM, &model->io_num, sizeof(model->io_num));
    if (ret != RKNN_SUCC) {
        printf("POSE_MODEL_QUERY_IO_FAIL model=%s ret=%d\n", model->name.c_str(), ret);
        release_pose_rknn_model(model);
        return -1;
    }
    printf("POSE_MODEL_READY model=%s path=%s inputs=%d outputs=%d\n",
           model->name.c_str(), model_path, model->io_num.n_input, model->io_num.n_output);

    model->input_attrs.resize(model->io_num.n_input);
    for (int i = 0; i < model->io_num.n_input; i++) {
        memset(&model->input_attrs[i], 0, sizeof(rknn_tensor_attr));
        model->input_attrs[i].index = i;
        ret = rknn_query(model->ctx, RKNN_QUERY_INPUT_ATTR, &model->input_attrs[i], sizeof(rknn_tensor_attr));
        if (ret != RKNN_SUCC) {
            printf("POSE_MODEL_QUERY_INPUT_FAIL model=%s index=%d ret=%d\n", model->name.c_str(), i, ret);
            release_pose_rknn_model(model);
            return -1;
        }
        dump_tensor_attr(model->name.c_str(), "input", &model->input_attrs[i]);
    }

    model->output_attrs.resize(model->io_num.n_output);
    for (int i = 0; i < model->io_num.n_output; i++) {
        memset(&model->output_attrs[i], 0, sizeof(rknn_tensor_attr));
        model->output_attrs[i].index = i;
        ret = rknn_query(model->ctx, RKNN_QUERY_OUTPUT_ATTR, &model->output_attrs[i], sizeof(rknn_tensor_attr));
        if (ret != RKNN_SUCC) {
            printf("POSE_MODEL_QUERY_OUTPUT_FAIL model=%s index=%d ret=%d\n", model->name.c_str(), i, ret);
            release_pose_rknn_model(model);
            return -1;
        }
        dump_tensor_attr(model->name.c_str(), "output", &model->output_attrs[i]);
    }

    model->is_quant = !model->output_attrs.empty() &&
                      model->output_attrs[0].qnt_type == RKNN_TENSOR_QNT_AFFINE_ASYMMETRIC &&
                      model->output_attrs[0].type == RKNN_TENSOR_INT8;
    infer_model_shape(model);
    printf("POSE_MODEL_INPUT_SHAPE model=%s width=%d height=%d channel=%d quant=%d\n",
           model->name.c_str(),
           model->model_width,
           model->model_height,
           model->model_channel,
           model->is_quant ? 1 : 0);

    if (model->io_num.n_input != 1 || model->model_width <= 0 ||
        model->model_height <= 0 || model->model_channel <= 0) {
        printf("POSE_MODEL_UNSUPPORTED_INPUT model=%s inputs=%d width=%d height=%d channel=%d\n",
               model->name.c_str(),
               model->io_num.n_input,
               model->model_width,
               model->model_height,
               model->model_channel);
        release_pose_rknn_model(model);
        return -1;
    }

    return 0;
}

int release_pose_rknn_model(pose_rknn_model_t *model)
{
    if (model == NULL) {
        return 0;
    }
    if (model->ctx != 0) {
        rknn_destroy(model->ctx);
        model->ctx = 0;
    }
    model->input_attrs.clear();
    model->output_attrs.clear();
    memset(&model->io_num, 0, sizeof(model->io_num));
    model->model_width = 0;
    model->model_height = 0;
    model->model_channel = 0;
    model->is_quant = false;
    return 0;
}

int run_pose_rknn_model_on_image(pose_rknn_model_t *model,
                                 image_buffer_t *image,
                                 bool dump_output_stats_enabled,
                                 pose_rknn_run_result_t *result)
{
    if (model == NULL || model->ctx == 0 || image == NULL || result == NULL) {
        return -1;
    }

    result->outputs.clear();
    memset(&result->letterbox, 0, sizeof(result->letterbox));

    image_buffer_t input_image;
    memset(&input_image, 0, sizeof(input_image));
    input_image.width = model->model_width;
    input_image.height = model->model_height;
    input_image.format = IMAGE_FORMAT_RGB888;
    input_image.size = get_image_size(&input_image);
    input_image.virt_addr = (unsigned char *)malloc(input_image.size);
    if (input_image.virt_addr == NULL) {
        printf("POSE_INPUT_ALLOC_FAIL model=%s bytes=%d\n", model->name.c_str(), input_image.size);
        return -1;
    }

    double stage_start = monotonic_ms();
    int ret = convert_image_with_letterbox(image, &input_image, &result->letterbox, 114);
    double letterbox_ms = monotonic_ms() - stage_start;
    if (ret < 0) {
        printf("POSE_LETTERBOX_FAIL model=%s ret=%d\n", model->name.c_str(), ret);
        free(input_image.virt_addr);
        return -1;
    }
    printf("POSE_LETTERBOX model=%s x_pad=%d y_pad=%d input=%dx%d resize=%dx%d scale=%.8f ms=%.2f\n",
           model->name.c_str(),
           result->letterbox.x_pad,
           result->letterbox.y_pad,
           result->letterbox.input_width,
           result->letterbox.input_height,
           result->letterbox.resize_width,
           result->letterbox.resize_height,
           result->letterbox.scale,
           letterbox_ms);

    std::vector<rknn_input> inputs(model->io_num.n_input);
    memset(inputs.data(), 0, inputs.size() * sizeof(rknn_input));
    inputs[0].index = 0;
    inputs[0].type = RKNN_TENSOR_UINT8;
    inputs[0].fmt = RKNN_TENSOR_NHWC;
    inputs[0].size = model->model_width * model->model_height * model->model_channel;
    inputs[0].buf = input_image.virt_addr;

    stage_start = monotonic_ms();
    ret = rknn_inputs_set(model->ctx, model->io_num.n_input, inputs.data());
    double inputs_set_ms = monotonic_ms() - stage_start;
    if (ret < 0) {
        printf("POSE_INPUTS_SET_FAIL model=%s ret=%d\n", model->name.c_str(), ret);
        free(input_image.virt_addr);
        return -1;
    }

    stage_start = monotonic_ms();
    ret = rknn_run(model->ctx, NULL);
    double run_ms = monotonic_ms() - stage_start;
    if (ret < 0) {
        printf("POSE_RUN_FAIL model=%s ret=%d\n", model->name.c_str(), ret);
        free(input_image.virt_addr);
        return -1;
    }

    std::vector<rknn_output> outputs(model->io_num.n_output);
    memset(outputs.data(), 0, outputs.size() * sizeof(rknn_output));
    for (int i = 0; i < model->io_num.n_output; i++) {
        outputs[i].index = i;
        outputs[i].want_float = 0;
    }

    stage_start = monotonic_ms();
    ret = rknn_outputs_get(model->ctx, model->io_num.n_output, outputs.data(), NULL);
    double outputs_get_ms = monotonic_ms() - stage_start;
    if (ret < 0) {
        printf("POSE_OUTPUTS_GET_FAIL model=%s ret=%d\n", model->name.c_str(), ret);
        free(input_image.virt_addr);
        return -1;
    }

    result->outputs.reserve(model->io_num.n_output);
    for (int i = 0; i < model->io_num.n_output; i++) {
        pose_rknn_output_t out;
        out.index = i;
        out.attr = model->output_attrs[i];
        if (outputs[i].buf != NULL && outputs[i].size > 0) {
            out.data.resize(outputs[i].size);
            memcpy(out.data.data(), outputs[i].buf, outputs[i].size);
            if (dump_output_stats_enabled) {
                dump_output_stats(model->name.c_str(), i, &model->output_attrs[i], outputs[i].buf, outputs[i].size);
            }
        }
        result->outputs.push_back(std::move(out));
    }

    rknn_outputs_release(model->ctx, model->io_num.n_output, outputs.data());
    free(input_image.virt_addr);

    printf("POSE_RUN_RESULT model=%s inputs_set_ms=%.2f run_ms=%.2f outputs_get_ms=%.2f outputs=%d\n",
           model->name.c_str(),
           inputs_set_ms,
           run_ms,
           outputs_get_ms,
           (int)result->outputs.size());
    return 0;
}
