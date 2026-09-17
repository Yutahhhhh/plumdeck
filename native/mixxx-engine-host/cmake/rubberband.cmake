# Mixxx's keylock links the pinned static RubberBand built by
# scripts/build-rubberband.sh instead of whatever the system provides.
set(JUNCTION_RB_ROOT "${CMAKE_CURRENT_LIST_DIR}/../build-deps/rubberband")
if(WIN32)
  set(JUNCTION_RB_LIBRARY "${JUNCTION_RB_ROOT}/build/rubberband.lib")
else()
  set(JUNCTION_RB_LIBRARY "${JUNCTION_RB_ROOT}/build/librubberband.a")
endif()
if(NOT EXISTS "${JUNCTION_RB_LIBRARY}")
  message(FATAL_ERROR "Build the pinned RubberBand dependency with scripts/build-rubberband.sh")
endif()
if(TARGET rubberband::rubberband)
  set_target_properties(rubberband::rubberband PROPERTIES IMPORTED_LOCATION "${JUNCTION_RB_LIBRARY}" INTERFACE_INCLUDE_DIRECTORIES "${JUNCTION_RB_ROOT}/source")
  if(WIN32)
    set_property(TARGET rubberband::rubberband APPEND PROPERTY INTERFACE_COMPILE_DEFINITIONS RUBBERBAND_STATIC)
  endif()
endif()
# mixxx-lib already links rubberband::rubberband (and therefore this static
# archive); only its own further dependencies need adding here.
target_link_libraries(plumdeck-mixxx-engine-host PRIVATE "${JUNCTION_samplerate_LIBRARY}")
if(APPLE)
  target_link_libraries(plumdeck-mixxx-engine-host PRIVATE "-framework Accelerate")
endif()
