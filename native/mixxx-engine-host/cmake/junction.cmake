# plumdeck Junction native subsystem.
#
# The bulk of Junction (protocol, clocks, authority, turn state,
# program routing, codec, transport) has no Mixxx dependency. It
# is compiled into `plumdeck-junction-core` so it can be unit tested without an
# audio device or the pinned engine, and linked into both the real Mixxx host
# and the protocol seam. Mixxx-specific glue lives outside this library.
#
# Pinned third-party dependencies are recorded in dependency-versions.json.

set(JUNCTION_DIR "${CMAKE_CURRENT_LIST_DIR}/../src/junction")

# Source list grows as modules land. Keep it explicit: a glob would silently
# drop a file from the real Mixxx target and only fail at link time.
set(JUNCTION_CORE_SOURCES
  "${JUNCTION_DIR}/runtime.cpp"
  "${JUNCTION_DIR}/manual_exchange.cpp"
  "${JUNCTION_DIR}/network_settings.cpp"
  "${JUNCTION_DIR}/media_transport.cpp"
  "${JUNCTION_DIR}/program_output.cpp"
  "${JUNCTION_DIR}/junction_input.cpp"
  "${JUNCTION_DIR}/producer_tap.cpp"
  "${JUNCTION_DIR}/audio_bridge.cpp"
  "${JUNCTION_DIR}/authority.cpp"
  "${JUNCTION_DIR}/ids.cpp"
  "${JUNCTION_DIR}/audio_clock.cpp"
  "${JUNCTION_DIR}/session_protocol.cpp"
)

add_library(plumdeck-junction-core STATIC ${JUNCTION_CORE_SOURCES})
set_target_properties(plumdeck-junction-core PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
target_include_directories(plumdeck-junction-core PUBLIC "${CMAKE_CURRENT_LIST_DIR}/../src")
find_package(OpenSSL REQUIRED)
target_link_libraries(plumdeck-junction-core PUBLIC Qt6::Core OpenSSL::Crypto)

# --- libopus: the music codec for the producer uplink (03 section 5) --------
find_path(JUNCTION_OPUS_INCLUDE opus/opus.h HINTS /opt/homebrew/opt/opus/include /usr/local/opt/opus/include)
find_library(JUNCTION_OPUS_LIBRARY opus HINTS /opt/homebrew/opt/opus/lib /usr/local/opt/opus/lib)
if(JUNCTION_OPUS_INCLUDE AND JUNCTION_OPUS_LIBRARY)
  target_include_directories(plumdeck-junction-core PUBLIC "${JUNCTION_OPUS_INCLUDE}")
  target_link_libraries(plumdeck-junction-core PUBLIC "${JUNCTION_OPUS_LIBRARY}")
  target_compile_definitions(plumdeck-junction-core PUBLIC PLUMDECK_JUNCTION_WITH_OPUS=1)
  message(STATUS "Junction: libopus at ${JUNCTION_OPUS_LIBRARY}")
else()
  message(WARNING "Junction: libopus not found; the Opus codec path is compiled out and reports itself unavailable")
endif()

# --- libdatachannel: ICE/DTLS/SRTP + WebSocket signalling (03 section 1) ----
# Built from the pinned checkout under build-deps/. Absent means the transport
# reports itself unavailable; it never silently degrades to an insecure path.
set(JUNCTION_LDC_ROOT "${CMAKE_CURRENT_LIST_DIR}/../build-deps/libdatachannel")
if(WIN32)
  find_package(LibDataChannel CONFIG REQUIRED)
  target_link_libraries(plumdeck-junction-core PUBLIC LibDataChannel::LibDataChannel)
  target_compile_definitions(plumdeck-junction-core PUBLIC PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL=1)
elseif(EXISTS "${JUNCTION_LDC_ROOT}/build-static/libdatachannel.a")
  target_include_directories(plumdeck-junction-core PUBLIC "${JUNCTION_LDC_ROOT}/include")
  target_link_libraries(plumdeck-junction-core PUBLIC
    "${JUNCTION_LDC_ROOT}/build-static/libdatachannel.a"
    "${JUNCTION_LDC_ROOT}/build-static/deps/libsrtp/libsrtp2.a"
    "${JUNCTION_LDC_ROOT}/build-static/deps/usrsctp/usrsctplib/libusrsctp.a")
  # libnice provides UDP, TCP and TLS TURN; the libjuice backend only handles UDP.
  foreach(ice_library nice gio-2.0 gobject-2.0 glib-2.0)
    find_library(JUNCTION_${ice_library}_LIBRARY NAMES ${ice_library} HINTS /opt/homebrew/lib REQUIRED)
    target_link_libraries(plumdeck-junction-core PUBLIC ${JUNCTION_${ice_library}_LIBRARY})
  endforeach()
  find_package(OpenSSL REQUIRED)
  target_link_libraries(plumdeck-junction-core PUBLIC OpenSSL::SSL OpenSSL::Crypto)
  target_compile_definitions(plumdeck-junction-core PUBLIC PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL=1 RTC_STATIC=1)
  message(STATUS "Junction: libdatachannel static at ${JUNCTION_LDC_ROOT}/build-static")
else()
  message(WARNING "Junction: libdatachannel not built; run scripts/build-junction-deps.sh. WebRTC transport reports itself unavailable")
endif()

if(WIN32)
  foreach(audio_library portaudio samplerate sndfile)
    find_library(JUNCTION_${audio_library}_LIBRARY NAMES ${audio_library} REQUIRED)
    target_link_libraries(plumdeck-junction-core PUBLIC ${JUNCTION_${audio_library}_LIBRARY})
  endforeach()
  foreach(audio_header portaudio.h samplerate.h sndfile.h)
    find_path(JUNCTION_${audio_header}_INCLUDE NAMES ${audio_header} REQUIRED)
    target_include_directories(plumdeck-junction-core PUBLIC ${JUNCTION_${audio_header}_INCLUDE})
  endforeach()
  target_compile_definitions(plumdeck-junction-core PUBLIC NOMINMAX)
  target_link_libraries(plumdeck-junction-core PUBLIC Crypt32)
else()
find_package(PkgConfig REQUIRED)
pkg_check_modules(JUNCTION_AUDIO REQUIRED portaudio-2.0 samplerate sndfile)
target_include_directories(plumdeck-junction-core PUBLIC ${JUNCTION_AUDIO_INCLUDE_DIRS})
# Link shared audio libraries directly: pkg-config's repeated -framework flags
# are otherwise deduplicated into invalid bare framework names by CMake.
foreach(audio_library portaudio samplerate sndfile)
  find_library(JUNCTION_${audio_library}_LIBRARY NAMES ${audio_library}
    HINTS ${JUNCTION_AUDIO_LIBRARY_DIRS} REQUIRED)
  target_link_libraries(plumdeck-junction-core PUBLIC ${JUNCTION_${audio_library}_LIBRARY})
endforeach()

endif()

if(APPLE)
  find_library(JUNCTION_IOKIT_FRAMEWORK IOKit REQUIRED)
  find_library(JUNCTION_COREFOUNDATION_FRAMEWORK CoreFoundation REQUIRED)
  find_library(JUNCTION_SECURITY_FRAMEWORK Security REQUIRED)
  target_link_libraries(plumdeck-junction-core PUBLIC "${JUNCTION_SECURITY_FRAMEWORK}" "${JUNCTION_IOKIT_FRAMEWORK}" "${JUNCTION_COREFOUNDATION_FRAMEWORK}")
endif()
