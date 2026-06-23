#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "image_utils.h"
#include "pose_model.h"

namespace {

struct Options {
    const char *detector_model_path = "model/pose_detector.rknn";
    const char *landmark_model_path = "model/pose_landmark_lite.rknn";
    const char *image_path = NULL;
    bool run_landmark = true;
    bool dump_output_stats = false;
};

void print_usage(const char *argv0)
{
    printf("Usage: %s [OPTIONS] <image_path>\n", argv0);
    printf("  --detector-model <PATH>    pose detector RKNN [default: model/pose_detector.rknn]\n");
    printf("  --landmark-model <PATH>    pose landmark RKNN [default: model/pose_landmark_lite.rknn]\n");
    printf("  --skip-landmark            only run detector model\n");
    printf("  --dump-output-stats        print raw output byte statistics\n");
}

bool parse_args(int argc, char **argv, Options *options)
{
    for (int i = 1; i < argc; ++i) {
        const char *arg = argv[i];
        const char *value = i + 1 < argc ? argv[i + 1] : NULL;

        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            print_usage(argv[0]);
            exit(0);
        }

        if (strcmp(arg, "--detector-model") == 0 && value != NULL) {
            options->detector_model_path = value;
            ++i;
        } else if (strcmp(arg, "--landmark-model") == 0 && value != NULL) {
            options->landmark_model_path = value;
            ++i;
        } else if (strcmp(arg, "--skip-landmark") == 0) {
            options->run_landmark = false;
        } else if (strcmp(arg, "--dump-output-stats") == 0) {
            options->dump_output_stats = true;
        } else if (arg[0] == '-') {
            printf("unknown or incomplete argument: %s\n", arg);
            return false;
        } else if (options->image_path == NULL) {
            options->image_path = arg;
        } else {
            printf("unexpected extra image argument: %s\n", arg);
            return false;
        }
    }

    if (options->image_path == NULL) {
        printf("missing image_path\n");
        return false;
    }
    return true;
}

void free_image(image_buffer_t *image)
{
    if (image != NULL && image->virt_addr != NULL) {
        free(image->virt_addr);
        image->virt_addr = NULL;
    }
}

}  // namespace

int main(int argc, char **argv)
{
    Options options;
    if (!parse_args(argc, argv, &options)) {
        print_usage(argv[0]);
        return 2;
    }

    printf("MediaPipe Pose RKNN image probe\n");
    printf("================================\n");
    printf("detector_model: %s\n", options.detector_model_path);
    printf("landmark_model: %s\n", options.run_landmark ? options.landmark_model_path : "(skipped)");
    printf("image: %s\n", options.image_path);

    image_buffer_t src_image;
    memset(&src_image, 0, sizeof(src_image));
    int ret = read_image(options.image_path, &src_image);
    if (ret != 0) {
        printf("POSE_IMAGE_READ_FAIL ret=%d image_path=%s\n", ret, options.image_path);
        return 1;
    }
    printf("POSE_IMAGE_READ_OK width=%d height=%d format=%d size=%d\n",
           src_image.width,
           src_image.height,
           src_image.format,
           src_image.size);

    pose_rknn_model_t detector;
    ret = init_pose_rknn_model(options.detector_model_path, "detector", &detector);
    if (ret != 0) {
        printf("POSE_DETECTOR_INIT_FAIL ret=%d\n", ret);
        free_image(&src_image);
        return 1;
    }

    pose_rknn_run_result_t detector_result;
    ret = run_pose_rknn_model_on_image(&detector, &src_image, options.dump_output_stats, &detector_result);
    if (ret != 0) {
        printf("POSE_DETECTOR_RUN_FAIL ret=%d\n", ret);
        release_pose_rknn_model(&detector);
        free_image(&src_image);
        return 1;
    }

    if (options.run_landmark) {
        pose_rknn_model_t landmark;
        ret = init_pose_rknn_model(options.landmark_model_path, "landmark", &landmark);
        if (ret != 0) {
            printf("POSE_LANDMARK_INIT_FAIL ret=%d\n", ret);
            release_pose_rknn_model(&detector);
            free_image(&src_image);
            return 1;
        }

        pose_rknn_run_result_t landmark_result;
        ret = run_pose_rknn_model_on_image(&landmark, &src_image, options.dump_output_stats, &landmark_result);
        if (ret != 0) {
            printf("POSE_LANDMARK_RUN_FAIL ret=%d\n", ret);
            release_pose_rknn_model(&landmark);
            release_pose_rknn_model(&detector);
            free_image(&src_image);
            return 1;
        }
        release_pose_rknn_model(&landmark);
    }

    release_pose_rknn_model(&detector);
    free_image(&src_image);

    printf("POSE_IMAGE_PROBE_DONE\n");
    return 0;
}
