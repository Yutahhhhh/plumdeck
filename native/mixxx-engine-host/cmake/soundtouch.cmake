# Mixxx's keylock links the pinned static SoundTouch built by
# scripts/build-soundtouch.sh instead of whatever the system provides.
set(JUNCTION_ST_ROOT "${CMAKE_CURRENT_LIST_DIR}/../build-deps/soundtouch")
if(WIN32)
  set(JUNCTION_ST_LIBRARY "${JUNCTION_ST_ROOT}/build/SoundTouch.lib")
else()
  set(JUNCTION_ST_LIBRARY "${JUNCTION_ST_ROOT}/build/libSoundTouch.a")
endif()
if(NOT EXISTS "${JUNCTION_ST_LIBRARY}")
  message(FATAL_ERROR "Build the pinned SoundTouch dependency with scripts/build-soundtouch.sh")
endif()
if(TARGET SoundTouch::SoundTouch)
  set_target_properties(SoundTouch::SoundTouch PROPERTIES IMPORTED_LOCATION "${JUNCTION_ST_LIBRARY}"
    INTERFACE_INCLUDE_DIRECTORIES "${JUNCTION_ST_ROOT}/include")
endif()
