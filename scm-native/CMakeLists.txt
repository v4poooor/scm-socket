cmake_minimum_required(VERSION 3.10)
project(scm_jni)
# 自动寻找 JNI
find_package(JNI REQUIRED)
# 包含头文件
include_directories(${JNI_INCLUDE_DIRS} include)
# 生成动态库 (Windows 下生成 .dll, Linux 下生成 .so)
add_library(scm_jni SHARED src/scm_jni.cpp)