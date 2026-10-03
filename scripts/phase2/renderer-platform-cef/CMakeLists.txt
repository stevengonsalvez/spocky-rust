# Build the Spocky CEF host against a pinned CEF binary distribution.
#   cmake -S . -B BUILD -DCEF_ROOT=/path/to/cef_binary_* -DCMAKE_BUILD_TYPE=Release
cmake_minimum_required(VERSION 3.21)
project(spocky_cef_host LANGUAGES C CXX OBJCXX)

if(NOT CEF_ROOT OR NOT EXISTS "${CEF_ROOT}/cmake/FindCEF.cmake")
  message(FATAL_ERROR "set -DCEF_ROOT to an unpacked cef_binary_* distribution")
endif()
set(CMAKE_MODULE_PATH "${CEF_ROOT}/cmake")
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT PROJECT_ARCH)
  set(PROJECT_ARCH "${CMAKE_SYSTEM_PROCESSOR}")
endif()

find_package(CEF REQUIRED)
add_subdirectory("${CEF_ROOT}/libcef_dll" libcef_dll_wrapper)

if(NOT OS_MAC)
  message(FATAL_ERROR "this build file covers macOS; Linux and Windows entry points are separate steps")
endif()

set(HOST_NAME "spocky-cef-host")
set(HOST_BUNDLE_ID "org.spocky.cefhost")

# Helper apps, one per CEF process type (GPU, renderer, plugin, alerts).
set(HELPER_TARGETS)
set(HELPER_APPS)
foreach(_suffix_list ${CEF_HELPER_APP_SUFFIXES})
  string(REPLACE ":" ";" _suffix_list ${_suffix_list})
  list(GET _suffix_list 0 _name_suffix)
  list(GET _suffix_list 1 _target_suffix)
  list(GET _suffix_list 2 _plist_suffix)
  set(_helper_target "${HOST_NAME}_helper${_target_suffix}")
  set(_helper_output "${HOST_NAME} Helper${_name_suffix}")
  set(_helper_info_plist "${CMAKE_CURRENT_BINARY_DIR}/helper-Info${_target_suffix}.plist")
  set(HELPER_EXECUTABLE_NAME "${_helper_output}")
  set(HELPER_BUNDLE_ID "${HOST_BUNDLE_ID}.helper${_plist_suffix}")
  configure_file("${CMAKE_CURRENT_SOURCE_DIR}/helper-Info.plist.in" "${_helper_info_plist}" @ONLY)
  add_executable(${_helper_target} MACOSX_BUNDLE helper_mac.cc)
  SET_EXECUTABLE_TARGET_PROPERTIES(${_helper_target})
  add_dependencies(${_helper_target} libcef_dll_wrapper)
  target_link_libraries(${_helper_target} libcef_dll_wrapper ${CEF_STANDARD_LIBS})
  set_target_properties(${_helper_target} PROPERTIES
    MACOSX_BUNDLE_INFO_PLIST "${_helper_info_plist}"
    OUTPUT_NAME "${_helper_output}")
  list(APPEND HELPER_TARGETS ${_helper_target})
  list(APPEND HELPER_APPS "${_helper_output}")
endforeach()

set(HOST_EXECUTABLE_NAME "${HOST_NAME}")
configure_file("${CMAKE_CURRENT_SOURCE_DIR}/Info.plist.in" "${CMAKE_CURRENT_BINARY_DIR}/Info.plist" @ONLY)
add_executable(${HOST_NAME} MACOSX_BUNDLE host.cc main_mac.mm)
SET_EXECUTABLE_TARGET_PROPERTIES(${HOST_NAME})
add_dependencies(${HOST_NAME} libcef_dll_wrapper ${HELPER_TARGETS})
target_link_libraries(${HOST_NAME} libcef_dll_wrapper ${CEF_STANDARD_LIBS})
set_target_properties(${HOST_NAME} PROPERTIES
  MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_BINARY_DIR}/Info.plist")

# Assemble the bundle: the framework and the helper apps live inside the host app.
COPY_MAC_FRAMEWORK(${HOST_NAME} "${CEF_BINARY_DIR}" "$<TARGET_BUNDLE_DIR:${HOST_NAME}>")
list(LENGTH HELPER_TARGETS _helper_count)
math(EXPR _last "${_helper_count} - 1")
foreach(_index RANGE ${_last})
  list(GET HELPER_TARGETS ${_index} _helper_target)
  list(GET HELPER_APPS ${_index} _helper_app)
  add_custom_command(TARGET ${HOST_NAME} POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_directory
      "$<TARGET_BUNDLE_DIR:${_helper_target}>"
      "$<TARGET_BUNDLE_DIR:${HOST_NAME}>/Contents/Frameworks/${_helper_app}.app"
    VERBATIM)
endforeach()
