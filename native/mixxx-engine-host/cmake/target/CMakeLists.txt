if(NOT TARGET mixxx-lib)
  message(FATAL_ERROR "plumdeck host requires the pinned upstream mixxx-lib target")
endif()
set(PLUMDECK_ROOT "${CMAKE_CURRENT_LIST_DIR}/../..")
# Extend the pinned built-in registry in the build tree; keep upstream clean.
set(PLUMDECK_REGISTRY_SOURCE "${CMAKE_SOURCE_DIR}/src/effects/backends/builtin/builtinbackend.cpp")
file(READ "${PLUMDECK_REGISTRY_SOURCE}" PLUMDECK_REGISTRY)
set(PLUMDECK_REGISTRATIONS "")
foreach(PLUMDECK_EFFECT RANGE 0 7)
  string(APPEND PLUMDECK_REGISTRATIONS "    registerEffect<DdjTimedProcessor<DdjBeatProcessor<${PLUMDECK_EFFECT}>>>();\n")
endforeach()
foreach(PLUMDECK_EFFECT Echo Reverb Tremolo Flanger Phaser PitchShift)
  string(APPEND PLUMDECK_REGISTRATIONS "    registerEffect<DdjTimedProcessor<${PLUMDECK_EFFECT}Effect>>();\n")
endforeach()
string(REPLACE "BuiltInBackend::BuiltInBackend() {" "BuiltInBackend::BuiltInBackend() {\n${PLUMDECK_REGISTRATIONS}" PLUMDECK_REGISTRY "${PLUMDECK_REGISTRY}")
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_builtinbackend.cpp" "#include \"${PLUMDECK_ROOT}/src/ddj_beat_processors.h\"\n${PLUMDECK_REGISTRY}")
get_target_property(PLUMDECK_MIXXX_SOURCES mixxx-lib SOURCES)
list(FILTER PLUMDECK_MIXXX_SOURCES EXCLUDE REGEX "effects/backends/builtin/builtinbackend\\.cpp$")
set_property(TARGET mixxx-lib PROPERTY SOURCES "${PLUMDECK_MIXXX_SOURCES}")
target_sources(mixxx-lib PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_builtinbackend.cpp")
add_executable(plumdeck-mixxx-engine-host
  "${PLUMDECK_ROOT}/src/main.cpp"
  "${PLUMDECK_ROOT}/src/host.cpp"
  "${PLUMDECK_ROOT}/src/mixxx_backend.cpp")
set_target_properties(plumdeck-mixxx-engine-host PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_definitions(plumdeck-mixxx-engine-host PRIVATE PLUMDECK_WITH_MIXXX=1)
target_include_directories(plumdeck-mixxx-engine-host PRIVATE "${CMAKE_SOURCE_DIR}/src" "${CMAKE_BINARY_DIR}/src")
target_link_libraries(plumdeck-mixxx-engine-host PRIVATE mixxx-lib mixxx-gitinfostore)

# Reproducible hook in the generated SoundManager translation unit; pinned
# checkout stays clean. The callback captures the final master exactly once.
set(PLUMDECK_SOUND_SOURCE "${CMAKE_SOURCE_DIR}/src/soundio/soundmanager.cpp")
file(READ "${PLUMDECK_SOUND_SOURCE}" PLUMDECK_SOUND)
set(PLUMDECK_SOUND_NEEDLE "m_pEngineMixer->process(iFramesPerBuffer * 2);")
string(FIND "${PLUMDECK_SOUND}" "${PLUMDECK_SOUND_NEEDLE}" PLUMDECK_SOUND_MATCH)
if(PLUMDECK_SOUND_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned SoundManager audio hook no longer matches")
endif()
string(REPLACE "${PLUMDECK_SOUND_NEEDLE}" "if (junction::beforeAudio(iFramesPerBuffer)) m_pEngineMixer->process(iFramesPerBuffer * 2);
    junction::afterAudio(const_cast<float*>(m_pEngineMixer->getMainBuffer()), const_cast<float*>(m_pEngineMixer->getHeadphoneBuffer()), iFramesPerBuffer);" PLUMDECK_SOUND "${PLUMDECK_SOUND}")
set(PLUMDECK_INPUT_NEEDLE "const SINT iFramesPerBuffer) {\n   for (QList<AudioInputBuffer>::ConstIterator")
string(FIND "${PLUMDECK_SOUND}" "${PLUMDECK_INPUT_NEEDLE}" PLUMDECK_INPUT_MATCH)
if(PLUMDECK_INPUT_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned SoundManager input ownership hook no longer matches")
endif()
string(REPLACE "${PLUMDECK_INPUT_NEEDLE}" "const SINT iFramesPerBuffer) {\n   junction::InputGuard inputGuard;\n   if (!inputGuard) return;\n   for (QList<AudioInputBuffer>::ConstIterator" PLUMDECK_SOUND "${PLUMDECK_SOUND}")
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_soundmanager.cpp" "#include \"${PLUMDECK_ROOT}/src/junction/audio_bridge.h\"\n${PLUMDECK_SOUND}")
get_target_property(PLUMDECK_MIXXX_SOURCES mixxx-lib SOURCES)
list(FILTER PLUMDECK_MIXXX_SOURCES EXCLUDE REGEX "soundio/soundmanager\\.cpp$")
set_property(TARGET mixxx-lib PROPERTY SOURCES "${PLUMDECK_MIXXX_SOURCES}")
target_sources(mixxx-lib PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_soundmanager.cpp")
include("${PLUMDECK_ROOT}/cmake/junction.cmake")
target_link_libraries(plumdeck-mixxx-engine-host PRIVATE plumdeck-junction-core)
# The injected SoundManager/EngineMixer sources live in mixxx-lib and are also
# linked by upstream's optional `mixxx` executable. Keep the default `all`
# target linkable as well as the PlumDeck host target.
if(TARGET mixxx)
  target_link_libraries(mixxx PRIVATE plumdeck-junction-core)
endif()

target_sources(plumdeck-mixxx-engine-host PRIVATE "${PLUMDECK_ROOT}/src/junction/private_preview.cpp")

target_include_directories(mixxx-lib PRIVATE "${CMAKE_SOURCE_DIR}/src/soundio")

file(READ "${CMAKE_SOURCE_DIR}/src/engine/enginemixer.cpp" PLUMDECK_MIXER)
string(FIND "${PLUMDECK_MIXER}" "const CSAMPLE* EngineMixer::buffer(const AudioOutput& output) const {" PLUMDECK_SINK_MATCH)
if(PLUMDECK_SINK_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned EngineMixer device sink hook no longer matches")
endif()
string(REPLACE "const CSAMPLE* EngineMixer::buffer(const AudioOutput& output) const {" "const CSAMPLE* EngineMixer::buffer(const AudioOutput& output) const {
    if (output.getType() == AudioPathType::Main || output.getType() == AudioPathType::Headphones) {
        if (const auto* idle = junction::idleOutput(output.getType() == AudioPathType::Headphones)) return idle;
    }" PLUMDECK_MIXER "${PLUMDECK_MIXER}")
set(PLUMDECK_CLOCK_NEEDLE "m_sampleRate = mixxx::audio::SampleRate::fromDouble(m_pSampleRate->get());")
string(FIND "${PLUMDECK_MIXER}" "${PLUMDECK_CLOCK_NEEDLE}" PLUMDECK_CLOCK_MATCH)
if(PLUMDECK_CLOCK_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned mixer clock hook no longer matches")
endif()
string(REPLACE "${PLUMDECK_CLOCK_NEEDLE}" "${PLUMDECK_CLOCK_NEEDLE}
    deckclock::beginAudio(iBufferSize / 2, m_sampleRate.value()); deckclock::CallbackScope plumdeckCallbackScope;" PLUMDECK_MIXER "${PLUMDECK_MIXER}")
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_enginemixer.cpp" "#include \"${PLUMDECK_ROOT}/src/junction/audio_bridge.h\"\n#include \"${PLUMDECK_ROOT}/src/deck_telemetry.h\"\n${PLUMDECK_MIXER}")
get_target_property(PLUMDECK_MIXXX_SOURCES mixxx-lib SOURCES)
list(FILTER PLUMDECK_MIXXX_SOURCES EXCLUDE REGEX "engine/enginemixer\\.cpp$")
set_property(TARGET mixxx-lib PROPERTY SOURCES "${PLUMDECK_MIXXX_SOURCES}")
target_sources(mixxx-lib PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_enginemixer.cpp")

file(READ "${CMAKE_SOURCE_DIR}/src/engine/enginebuffer.cpp" PLUMDECK_BUFFER)
foreach(PLUMDECK_NEEDLE "m_playPos = position;" "m_pReadAheadManager->getFilePlaypositionFromLog(m_playPos, framesRead);")
  string(FIND "${PLUMDECK_BUFFER}" "${PLUMDECK_NEEDLE}" PLUMDECK_MATCH)
  if(PLUMDECK_MATCH EQUAL -1)
    message(FATAL_ERROR "Pinned EngineBuffer boundary hook no longer matches")
  endif()
endforeach()
string(REPLACE "m_playPos = position;" "deckclock::boundary(1);
    m_playPos = position;" PLUMDECK_BUFFER "${PLUMDECK_BUFFER}")
string(REPLACE "// Adjust filepos_play by the amount we processed." "// Adjust filepos_play by the amount we processed.
            const auto expectedPosition = m_playPos + framesRead;" PLUMDECK_BUFFER "${PLUMDECK_BUFFER}")
string(REPLACE "m_pReadAheadManager->getFilePlaypositionFromLog(m_playPos, framesRead);" "m_pReadAheadManager->getFilePlaypositionFromLog(m_playPos, framesRead);
            if (m_playPos != expectedPosition) deckclock::boundary(2);" PLUMDECK_BUFFER "${PLUMDECK_BUFFER}")
set(PLUMDECK_SCALER_NEEDLE "m_pScaleLinear = new EngineBufferScaleLinear(m_pReadAheadManager);")
string(FIND "${PLUMDECK_BUFFER}" "${PLUMDECK_SCALER_NEEDLE}" PLUMDECK_SCALER_MATCH)
if(PLUMDECK_SCALER_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned vinyl scaler hook no longer matches")
endif()
string(REPLACE "${PLUMDECK_SCALER_NEEDLE}" "m_pScaleLinear = qEnvironmentVariable(\"PLUMDECK_VINYL_SCALER\") != \"linear\" ? static_cast<EngineBufferScaleLinear*>(new PlumdeckVinylScaler(m_pReadAheadManager)) : new EngineBufferScaleLinear(m_pReadAheadManager);" PLUMDECK_BUFFER "${PLUMDECK_BUFFER}")
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_enginebuffer.cpp" "#include \"${PLUMDECK_ROOT}/src/deck_telemetry.h\"\n#include \"${PLUMDECK_ROOT}/src/plumdeck_vinyl_scaler.h\"\n${PLUMDECK_BUFFER}")
get_target_property(PLUMDECK_MIXXX_SOURCES mixxx-lib SOURCES)
list(FILTER PLUMDECK_MIXXX_SOURCES EXCLUDE REGEX "engine/enginebuffer\\.cpp$")
set_property(TARGET mixxx-lib PROPERTY SOURCES "${PLUMDECK_MIXXX_SOURCES}")
target_sources(mixxx-lib PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_enginebuffer.cpp")

target_include_directories(mixxx-lib PRIVATE "${CMAKE_SOURCE_DIR}/src/engine")

file(READ "${CMAKE_SOURCE_DIR}/src/engine/cachingreader/cachingreaderworker.cpp" PLUMDECK_READER)
set(PLUMDECK_READER_NEEDLE "m_pAudioSource = SoundSourceProxy(pTrack).openAudioSource(config);")
string(FIND "${PLUMDECK_READER}" "${PLUMDECK_READER_NEEDLE}" PLUMDECK_READER_MATCH)
if(PLUMDECK_READER_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned decoder binding hook no longer matches")
endif()
string(REPLACE "${PLUMDECK_READER_NEEDLE}" "SoundSourceProxy plumdeckProxy(pTrack);
    m_pAudioSource = plumdeckProxy.openAudioSource(config);
    waveform::bindSource(pTrack->getLocation(), plumdeckProxy, m_pAudioSource);" PLUMDECK_READER "${PLUMDECK_READER}")
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_cachingreaderworker.cpp" "#include \"${PLUMDECK_ROOT}/src/waveform/source_binding.h\"\n${PLUMDECK_READER}")
get_target_property(PLUMDECK_MIXXX_SOURCES mixxx-lib SOURCES)
list(FILTER PLUMDECK_MIXXX_SOURCES EXCLUDE REGEX "engine/cachingreader/cachingreaderworker\\.cpp$")
set_property(TARGET mixxx-lib PROPERTY SOURCES "${PLUMDECK_MIXXX_SOURCES}")
target_sources(mixxx-lib PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_cachingreaderworker.cpp")
target_include_directories(mixxx-lib PRIVATE "${CMAKE_SOURCE_DIR}/src/engine/cachingreader")
target_sources(plumdeck-mixxx-engine-host PRIVATE "${PLUMDECK_ROOT}/src/waveform/manager.cpp")

target_link_libraries(plumdeck-mixxx-engine-host PRIVATE Qt6::Network)

target_sources(mixxx-lib PRIVATE "${PLUMDECK_ROOT}/src/plumdeck_vinyl_scaler.cpp")

# Observe actual PortAudio underflow/overflow reports without logging in RT.
file(READ "${CMAKE_SOURCE_DIR}/src/soundio/sounddeviceportaudio.cpp" PLUMDECK_PORTAUDIO)
string(FIND "${PLUMDECK_PORTAUDIO}" "m_pSoundManager->underflowHappened(" PLUMDECK_XRUN_MATCH)
if(PLUMDECK_XRUN_MATCH EQUAL -1)
  message(FATAL_ERROR "Pinned PortAudio xrun hook no longer matches")
endif()
string(REPLACE "m_pSoundManager->underflowHappened(" "deckclock::xruns.fetch_add(1, std::memory_order_relaxed), m_pSoundManager->underflowHappened(" PLUMDECK_PORTAUDIO "${PLUMDECK_PORTAUDIO}")
if(WIN32)
  # Keep the engine/hand-off clock at 44.1 kHz while WASAPI converts to the
  # Windows shared mixer format (commonly 48 kHz). ASIO remains untouched.
  set(PLUMDECK_WASAPI_NEEDLE "    PaStream *pStream;")
  string(FIND "${PLUMDECK_PORTAUDIO}" "${PLUMDECK_WASAPI_NEEDLE}" PLUMDECK_WASAPI_MATCH)
  if(PLUMDECK_WASAPI_MATCH EQUAL -1)
    message(FATAL_ERROR "Pinned WASAPI stream configuration hook no longer matches")
  endif()
  string(REPLACE "${PLUMDECK_WASAPI_NEEDLE}" "
    PaWasapiStreamInfo plumdeckWasapi{};
    if (m_deviceTypeId == paWASAPI) {
        plumdeckWasapi.size = sizeof(plumdeckWasapi);
        plumdeckWasapi.hostApiType = paWASAPI;
        plumdeckWasapi.version = 1;
        plumdeckWasapi.flags = paWinWasapiAutoConvert;
        if (pOutputParams) pOutputParams->hostApiSpecificStreamInfo = &plumdeckWasapi;
        if (pInputParams) pInputParams->hostApiSpecificStreamInfo = &plumdeckWasapi;
    }
${PLUMDECK_WASAPI_NEEDLE}" PLUMDECK_PORTAUDIO "${PLUMDECK_PORTAUDIO}")
  string(PREPEND PLUMDECK_PORTAUDIO "#include <pa_win_wasapi.h>\n")
endif()
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_sounddeviceportaudio.cpp" "#include \"${PLUMDECK_ROOT}/src/deck_telemetry.h\"\n${PLUMDECK_PORTAUDIO}")
get_target_property(PLUMDECK_MIXXX_SOURCES mixxx-lib SOURCES)
list(FILTER PLUMDECK_MIXXX_SOURCES EXCLUDE REGEX "soundio/sounddeviceportaudio\\.cpp$")
set_property(TARGET mixxx-lib PROPERTY SOURCES "${PLUMDECK_MIXXX_SOURCES}")
target_sources(mixxx-lib PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/plumdeck_sounddeviceportaudio.cpp")

include("${PLUMDECK_ROOT}/cmake/keylock-checkpoint.cmake")
