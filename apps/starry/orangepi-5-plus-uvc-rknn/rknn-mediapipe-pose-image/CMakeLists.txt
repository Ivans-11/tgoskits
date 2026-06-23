cmake_minimum_required(VERSION 3.15)

project(rknn_mediapipe_pose_image C CXX)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_INSTALL_RPATH "$ORIGIN/lib")

set(POSE_IMAGE_ROOT ${CMAKE_CURRENT_SOURCE_DIR})
set(YOLO_IMAGE_ROOT ${POSE_IMAGE_ROOT}/../rknn-yolov8-image)
set(TARGET_SOC rk3588 CACHE STRING "Rockchip target SoC")

add_subdirectory(${YOLO_IMAGE_ROOT}/3rdparty ${CMAKE_CURRENT_BINARY_DIR}/3rdparty)
add_subdirectory(${YOLO_IMAGE_ROOT}/utils ${CMAKE_CURRENT_BINARY_DIR}/utils)

add_executable(rknn_pose_image
    cpp/pose_image_main.cc
    cpp/pose_model.cc
)

target_include_directories(rknn_pose_image PRIVATE
    ${POSE_IMAGE_ROOT}/cpp
    ${YOLO_IMAGE_ROOT}/utils
    ${LIBRKNNRT_INCLUDES}
)

target_link_libraries(rknn_pose_image
    imageutils
    fileutils
    ${LIBRKNNRT}
    dl
)

if (CMAKE_SYSTEM_NAME STREQUAL "Linux")
    set(THREADS_PREFER_PTHREAD_FLAG ON)
    find_package(Threads REQUIRED)
    target_link_libraries(rknn_pose_image Threads::Threads)
endif()

install(TARGETS rknn_pose_image DESTINATION .)
install(DIRECTORY model DESTINATION .)
install(DIRECTORY validation DESTINATION . OPTIONAL)
