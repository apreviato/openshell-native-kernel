cmake_minimum_required(VERSION 3.21)
project(OpenShellCefHost LANGUAGES C CXX)

if(NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
  message(FATAL_ERROR "OpenShell CEF host targets Linux")
endif()
if(NOT CEF_ROOT)
  message(FATAL_ERROR "Set CEF_ROOT to an extracted Linux CEF SDK")
endif()
if(NOT EXISTS "${OPENSHELL_CEF_POLICY_LIBRARY}")
  message(FATAL_ERROR "Build the Rust shell with tools/desktop/build-cef.sh")
endif()

list(APPEND CMAKE_MODULE_PATH "${CEF_ROOT}/cmake")
find_package(CEF REQUIRED)
find_package(Threads REQUIRED)
find_package(X11 REQUIRED)
if(NOT USE_SANDBOX)
  message(FATAL_ERROR "OpenShell requires the Chromium sandbox")
endif()

add_subdirectory("${CEF_LIBCEF_DLL_WRAPPER_PATH}" libcef_dll_wrapper)
ADD_LOGICAL_TARGET(openshell_libcef "${CEF_LIB_DEBUG}" "${CEF_LIB_RELEASE}")
add_library(openshell_cef_policy STATIC IMPORTED)
set_target_properties(openshell_cef_policy PROPERTIES
  IMPORTED_LOCATION "${OPENSHELL_CEF_POLICY_LIBRARY}"
  INTERFACE_LINK_LIBRARIES "Threads::Threads;${CMAKE_DL_LIBS};m;rt;util"
)

set(output "${CMAKE_BINARY_DIR}/runtime")
add_executable(openshell-cef-host main.cc session.cc browser_features.cc clipboard.cc input_state.cc)
SET_EXECUTABLE_TARGET_PROPERTIES(openshell-cef-host)
target_compile_features(openshell-cef-host PRIVATE cxx_std_17)
target_link_libraries(openshell-cef-host PRIVATE
  openshell_libcef libcef_dll_wrapper openshell_cef_policy ${CEF_STANDARD_LIBS} X11::X11 X11::Xfixes X11::Xss
)
set_target_properties(openshell-cef-host PROPERTIES
  RUNTIME_OUTPUT_DIRECTORY "${output}"
  BUILD_WITH_INSTALL_RPATH TRUE
  INSTALL_RPATH "$ORIGIN"
)

COPY_FILES(openshell-cef-host "${CEF_BINARY_FILES}" "${CEF_BINARY_DIR}" "${output}")
COPY_FILES(openshell-cef-host "${CEF_RESOURCE_FILES}" "${CEF_RESOURCE_DIR}" "${output}")
if(EXISTS "${CEF_BINARY_DIR}/libminigbm.so")
  COPY_FILES(openshell-cef-host libminigbm.so "${CEF_BINARY_DIR}" "${output}")
endif()
