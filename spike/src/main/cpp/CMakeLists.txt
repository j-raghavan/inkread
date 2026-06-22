# Native ebc helper for the RR19-FR4b pen-latency spike.
# A single small C file providing JNI entry points that open /dev/ebc and issue the
# Rockchip ebc-dev ioctls. Kotlin cannot ioctl(); this is the minimal bridge.
cmake_minimum_required(VERSION 3.22.1)
project(penspike_ebc C)

add_library(penspike_ebc SHARED ebc_jni.c)

# log for __android_log_print; the helper links nothing else.
find_library(log-lib log)
target_link_libraries(penspike_ebc ${log-lib})
