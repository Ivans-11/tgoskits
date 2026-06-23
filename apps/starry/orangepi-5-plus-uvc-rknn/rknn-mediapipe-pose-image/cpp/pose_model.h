#ifndef RKNN_MEDIAPIPE_POSE_MODEL_H_
#define RKNN_MEDIAPIPE_POSE_MODEL_H_

#include <stddef.h>
#include <stdint.h>

#include <string>
#include <vector>

#include "common.h"
#include "image_utils.h"
#include "rknn_api.h"

struct pose_rknn_output_t {
    int index = 0;
    rknn_tensor_attr attr;
    std::vector<unsigned char> data;
};

struct pose_rknn_run_result_t {
    letterbox_t letterbox;
    std::vector<pose_rknn_output_t> outputs;
};

struct pose_rknn_model_t {
    std::string name;
    rknn_context ctx = 0;
    rknn_input_output_num io_num = {};
    std::vector<rknn_tensor_attr> input_attrs;
    std::vector<rknn_tensor_attr> output_attrs;
    int model_width = 0;
    int model_height = 0;
    int model_channel = 0;
    bool is_quant = false;
};

int init_pose_rknn_model(const char *model_path, const char *name, pose_rknn_model_t *model);
int release_pose_rknn_model(pose_rknn_model_t *model);
int run_pose_rknn_model_on_image(pose_rknn_model_t *model,
                                 image_buffer_t *image,
                                 bool dump_output_stats,
                                 pose_rknn_run_result_t *result);

#endif  // RKNN_MEDIAPIPE_POSE_MODEL_H_
