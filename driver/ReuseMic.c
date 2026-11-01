// ReuseMic: a minimal Core Audio AudioServerPlugIn (HAL plug-in).
//
// Publishes two devices that share one ring buffer and one clock:
//   * "ReuseMic"       - an input-only virtual microphone apps can select.
//   * "ReuseMic Feed"  - a hidden output-only device. The reusemic app writes the
//                        enhanced audio here, and it comes out of "ReuseMic".
//
// Both devices are 48 kHz, mono, Float32. Sample times of both devices are derived
// from the same host-time anchor, so a frame written at sample time T on the feed
// is read back at sample time T on the mic.

#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>

// ---------------------------------------------------------------------------
// Configuration

#define kSampleRate 48000.0
#define kChannels 1
#define kBytesPerFrame (sizeof(Float32) * kChannels)
#define kRingFrames 16384 // also the zero time stamp period (~341 ms)

#define kManufacturer CFSTR("reusemic")
#define kMicName CFSTR("ReuseMic")
#define kMicUID CFSTR("ReuseMic_Device_UID")
#define kFeedName CFSTR("ReuseMic Feed")
#define kFeedUID CFSTR("ReuseMic_Feed_UID")
#define kModelUID CFSTR("ReuseMic_Model_UID")

enum {
    kObjectID_PlugIn = kAudioObjectPlugInObject, // 1
    kObjectID_Mic = 2,
    kObjectID_MicStream = 3,
    kObjectID_Feed = 4,
    kObjectID_FeedStream = 5,
};

// ---------------------------------------------------------------------------
// State

static AudioServerPlugInHostRef gHost = NULL;
static UInt32 gRefCount = 0;
static pthread_mutex_t gMutex = PTHREAD_MUTEX_INITIALIZER;

static Float64 gTicksPerFrame = 0;
static UInt64 gAnchorHostTime = 0;
static UInt32 gIOCount[2] = {0, 0}; // [0] mic, [1] feed

static Float32 gRing[kRingFrames * kChannels];
// End (exclusive) sample time of the most recent feed write; INT64_MIN when none.
static _Atomic int64_t gWriteEnd = INT64_MIN;

static int DeviceIndex(AudioObjectID d) { return d == kObjectID_Mic ? 0 : d == kObjectID_Feed ? 1 : -1; }
static int IsDevice(AudioObjectID o) { return o == kObjectID_Mic || o == kObjectID_Feed; }
static int IsStream(AudioObjectID o) { return o == kObjectID_MicStream || o == kObjectID_FeedStream; }
static AudioObjectID StreamOf(AudioObjectID d) { return d == kObjectID_Mic ? kObjectID_MicStream : kObjectID_FeedStream; }
static AudioObjectID DeviceOf(AudioObjectID s) { return s == kObjectID_MicStream ? kObjectID_Mic : kObjectID_Feed; }
static int IsInput(AudioObjectID o) { return o == kObjectID_Mic || o == kObjectID_MicStream; }

static AudioStreamBasicDescription Format(void) {
    AudioStreamBasicDescription f;
    memset(&f, 0, sizeof(f));
    f.mSampleRate = kSampleRate;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian | kAudioFormatFlagIsPacked;
    f.mBytesPerPacket = kBytesPerFrame;
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = kBytesPerFrame;
    f.mChannelsPerFrame = kChannels;
    f.mBitsPerChannel = 32;
    return f;
}

// ---------------------------------------------------------------------------
// Property helpers.
//
// Prop() both answers GetPropertyData (out != NULL) and GetPropertyDataSize
// (out == NULL), and HasProperty is "Prop() does not fail with UnknownProperty".

#define PUT(T, v)                                                                 \
    do {                                                                          \
        if (out) {                                                                \
            if (cap < sizeof(T)) return kAudioHardwareBadPropertySizeError;       \
            *(T *)out = (v);                                                      \
        }                                                                         \
        *size = sizeof(T);                                                        \
        return kAudioHardwareNoError;                                             \
    } while (0)

static OSStatus PutIDs(const AudioObjectID *ids, UInt32 n, UInt32 cap, UInt32 *size, void *out) {
    if (out) {
        UInt32 fit = cap / sizeof(AudioObjectID);
        if (fit < n) n = fit;
        memcpy(out, ids, n * sizeof(AudioObjectID));
    }
    *size = n * sizeof(AudioObjectID);
    return kAudioHardwareNoError;
}

static OSStatus PlugInProp(const AudioObjectPropertyAddress *a, UInt32 qs, const void *q, UInt32 cap, UInt32 *size,
                           void *out) {
    static const AudioObjectID devices[] = {kObjectID_Mic, kObjectID_Feed};
    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID);
    case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioPlugInClassID);
    case kAudioObjectPropertyOwner: PUT(AudioObjectID, kAudioObjectUnknown);
    case kAudioObjectPropertyManufacturer: PUT(CFStringRef, kManufacturer);
    case kAudioObjectPropertyOwnedObjects:
    case kAudioPlugInPropertyDeviceList: return PutIDs(devices, 2, cap, size, out);
    case kAudioPlugInPropertyTranslateUIDToDevice: {
        AudioObjectID id = kAudioObjectUnknown;
        if (out) {
            if (qs != sizeof(CFStringRef) || q == NULL) return kAudioHardwareBadPropertySizeError;
            CFStringRef uid = *(CFStringRef const *)q;
            if (uid && CFStringCompare(uid, kMicUID, 0) == kCFCompareEqualTo) id = kObjectID_Mic;
            else if (uid && CFStringCompare(uid, kFeedUID, 0) == kCFCompareEqualTo) id = kObjectID_Feed;
        }
        PUT(AudioObjectID, id);
    }
    case kAudioPlugInPropertyResourceBundle: PUT(CFStringRef, CFSTR(""));
    default: return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus DeviceProp(AudioObjectID dev, const AudioObjectPropertyAddress *a, UInt32 cap, UInt32 *size,
                           void *out) {
    int input = IsInput(dev);
    AudioObjectID stream = StreamOf(dev);
    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID);
    case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioDeviceClassID);
    case kAudioObjectPropertyOwner: PUT(AudioObjectID, kObjectID_PlugIn);
    case kAudioObjectPropertyName: PUT(CFStringRef, input ? kMicName : kFeedName);
    case kAudioObjectPropertyManufacturer: PUT(CFStringRef, kManufacturer);
    case kAudioDevicePropertyDeviceUID: PUT(CFStringRef, input ? kMicUID : kFeedUID);
    case kAudioDevicePropertyModelUID: PUT(CFStringRef, kModelUID);
    case kAudioDevicePropertyTransportType: PUT(UInt32, kAudioDeviceTransportTypeVirtual);
    case kAudioDevicePropertyRelatedDevices: {
        AudioObjectID self = dev;
        return PutIDs(&self, 1, cap, size, out);
    }
    case kAudioDevicePropertyClockDomain: PUT(UInt32, 0);
    case kAudioDevicePropertyDeviceIsAlive: PUT(UInt32, 1);
    case kAudioDevicePropertyDeviceIsRunning: {
        pthread_mutex_lock(&gMutex);
        UInt32 running = gIOCount[DeviceIndex(dev)] > 0;
        pthread_mutex_unlock(&gMutex);
        PUT(UInt32, running);
    }
    case kAudioDevicePropertyDeviceCanBeDefaultDevice:
    case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: {
        // Only the mic can be a default, and only on its own (input) side.
        UInt32 ok = input && a->mScope != kAudioObjectPropertyScopeOutput;
        PUT(UInt32, ok);
    }
    case kAudioDevicePropertyLatency: PUT(UInt32, 0);
    case kAudioDevicePropertySafetyOffset: PUT(UInt32, 0);
    case kAudioDevicePropertyIsHidden: PUT(UInt32, input ? 0 : 1);
    case kAudioDevicePropertyZeroTimeStampPeriod: PUT(UInt32, kRingFrames);
    case kAudioDevicePropertyNominalSampleRate: PUT(Float64, kSampleRate);
    case kAudioDevicePropertyAvailableNominalSampleRates: {
        AudioValueRange r = {kSampleRate, kSampleRate};
        PUT(AudioValueRange, r);
    }
    case kAudioObjectPropertyControlList: *size = 0; return kAudioHardwareNoError;
    case kAudioObjectPropertyOwnedObjects:
    case kAudioDevicePropertyStreams: {
        int match = a->mScope == kAudioObjectPropertyScopeGlobal ||
                    a->mScope == (input ? kAudioObjectPropertyScopeInput : kAudioObjectPropertyScopeOutput);
        return PutIDs(&stream, match ? 1 : 0, cap, size, out);
    }
    case kAudioDevicePropertyPreferredChannelsForStereo: {
        UInt32 ch[2] = {1, 1};
        if (out) {
            if (cap < sizeof(ch)) return kAudioHardwareBadPropertySizeError;
            memcpy(out, ch, sizeof(ch));
        }
        *size = sizeof(ch);
        return kAudioHardwareNoError;
    }
    case kAudioDevicePropertyPreferredChannelLayout: {
        UInt32 bytes = (UInt32)(offsetof(AudioChannelLayout, mChannelDescriptions) +
                                kChannels * sizeof(AudioChannelDescription));
        if (out) {
            if (cap < bytes) return kAudioHardwareBadPropertySizeError;
            AudioChannelLayout *l = (AudioChannelLayout *)out;
            memset(l, 0, bytes);
            l->mChannelLayoutTag = kAudioChannelLayoutTag_UseChannelDescriptions;
            l->mNumberChannelDescriptions = kChannels;
            l->mChannelDescriptions[0].mChannelLabel = kAudioChannelLabel_Mono;
        }
        *size = bytes;
        return kAudioHardwareNoError;
    }
    default: return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus StreamProp(AudioObjectID s, const AudioObjectPropertyAddress *a, UInt32 cap, UInt32 *size, void *out) {
    int input = IsInput(s);
    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID);
    case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioStreamClassID);
    case kAudioObjectPropertyOwner: PUT(AudioObjectID, DeviceOf(s));
    case kAudioObjectPropertyOwnedObjects: *size = 0; return kAudioHardwareNoError;
    case kAudioObjectPropertyName: PUT(CFStringRef, input ? kMicName : kFeedName);
    case kAudioStreamPropertyIsActive: PUT(UInt32, 1);
    case kAudioStreamPropertyDirection: PUT(UInt32, input ? 1 : 0);
    case kAudioStreamPropertyTerminalType:
        PUT(UInt32, input ? kAudioStreamTerminalTypeMicrophone : kAudioStreamTerminalTypeSpeaker);
    case kAudioStreamPropertyStartingChannel: PUT(UInt32, 1);
    case kAudioStreamPropertyLatency: PUT(UInt32, 0);
    case kAudioStreamPropertyVirtualFormat:
    case kAudioStreamPropertyPhysicalFormat: PUT(AudioStreamBasicDescription, Format());
    case kAudioStreamPropertyAvailableVirtualFormats:
    case kAudioStreamPropertyAvailablePhysicalFormats: {
        AudioStreamRangedDescription r;
        r.mFormat = Format();
        r.mSampleRateRange.mMinimum = kSampleRate;
        r.mSampleRateRange.mMaximum = kSampleRate;
        PUT(AudioStreamRangedDescription, r);
    }
    default: return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus Prop(AudioObjectID obj, const AudioObjectPropertyAddress *a, UInt32 qs, const void *q, UInt32 cap,
                     UInt32 *size, void *out) {
    if (a == NULL || size == NULL) return kAudioHardwareIllegalOperationError;
    if (obj == kObjectID_PlugIn) return PlugInProp(a, qs, q, cap, size, out);
    if (IsDevice(obj)) return DeviceProp(obj, a, cap, size, out);
    if (IsStream(obj)) return StreamProp(obj, a, cap, size, out);
    return kAudioHardwareBadObjectError;
}

// ---------------------------------------------------------------------------
// Driver interface

static HRESULT ReuseMic_QueryInterface(void *drv, REFIID uuid, LPVOID *out);
static ULONG ReuseMic_AddRef(void *drv);
static ULONG ReuseMic_Release(void *drv);

static OSStatus ReuseMic_Initialize(AudioServerPlugInDriverRef drv, AudioServerPlugInHostRef host) {
    (void)drv;
    gHost = host;
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    Float64 ticksPerSecond = 1e9 * (Float64)tb.denom / (Float64)tb.numer;
    gTicksPerFrame = ticksPerSecond / kSampleRate;
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc,
                                      const AudioServerPlugInClientInfo *c, AudioObjectID *out) {
    (void)d; (void)desc; (void)c; (void)out;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus ReuseMic_DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID dev) {
    (void)d; (void)dev;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus ReuseMic_AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID dev,
                                         const AudioServerPlugInClientInfo *c) {
    (void)d; (void)c;
    return IsDevice(dev) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static OSStatus ReuseMic_RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID dev,
                                            const AudioServerPlugInClientInfo *c) {
    (void)d; (void)c;
    return IsDevice(dev) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static OSStatus ReuseMic_PerformConfigChange(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt64 action,
                                             void *info) {
    (void)d; (void)dev; (void)action; (void)info;
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_AbortConfigChange(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt64 action,
                                           void *info) {
    (void)d; (void)dev; (void)action; (void)info;
    return kAudioHardwareNoError;
}

static Boolean ReuseMic_HasProperty(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid,
                                    const AudioObjectPropertyAddress *a) {
    (void)d; (void)pid;
    UInt32 size = 0;
    // TranslateUIDToDevice needs a qualifier to answer; it exists regardless.
    if (obj == kObjectID_PlugIn && a && a->mSelector == kAudioPlugInPropertyTranslateUIDToDevice) return true;
    return Prop(obj, a, 0, NULL, 0, &size, NULL) == kAudioHardwareNoError;
}

static OSStatus ReuseMic_IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid,
                                            const AudioObjectPropertyAddress *a, Boolean *settable) {
    if (!ReuseMic_HasProperty(d, obj, pid, a)) return kAudioHardwareUnknownPropertyError;
    if (settable) *settable = false;
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid,
                                             const AudioObjectPropertyAddress *a, UInt32 qs, const void *q,
                                             UInt32 *outSize) {
    (void)d; (void)pid;
    if (obj == kObjectID_PlugIn && a && a->mSelector == kAudioPlugInPropertyTranslateUIDToDevice) {
        *outSize = sizeof(AudioObjectID);
        return kAudioHardwareNoError;
    }
    return Prop(obj, a, qs, q, 0, outSize, NULL);
}

static OSStatus ReuseMic_GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid,
                                         const AudioObjectPropertyAddress *a, UInt32 qs, const void *q,
                                         UInt32 inSize, UInt32 *outSize, void *out) {
    (void)d; (void)pid;
    if (out == NULL) return kAudioHardwareIllegalOperationError;
    return Prop(obj, a, qs, q, inSize, outSize, out);
}

static OSStatus ReuseMic_SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid,
                                         const AudioObjectPropertyAddress *a, UInt32 qs, const void *q,
                                         UInt32 inSize, const void *in) {
    (void)d; (void)pid; (void)qs; (void)q;
    if (!ReuseMic_HasProperty(d, obj, pid, a)) return kAudioHardwareUnknownPropertyError;
    // Accept writes that ask for what we already are (some apps "set" 48 kHz).
    if (a->mSelector == kAudioDevicePropertyNominalSampleRate && inSize >= sizeof(Float64) &&
        *(const Float64 *)in == kSampleRate)
        return kAudioHardwareNoError;
    if ((a->mSelector == kAudioStreamPropertyVirtualFormat || a->mSelector == kAudioStreamPropertyPhysicalFormat) &&
        inSize >= sizeof(AudioStreamBasicDescription)) {
        AudioStreamBasicDescription want = *(const AudioStreamBasicDescription *)in, have = Format();
        if (want.mSampleRate == have.mSampleRate && want.mFormatID == have.mFormatID &&
            want.mChannelsPerFrame == have.mChannelsPerFrame && want.mBitsPerChannel == have.mBitsPerChannel)
            return kAudioHardwareNoError;
        return kAudioDeviceUnsupportedFormatError;
    }
    return kAudioHardwareUnsupportedOperationError;
}

static void NotifyRunning(AudioObjectID dev) {
    if (!gHost) return;
    AudioObjectPropertyAddress a = {kAudioDevicePropertyDeviceIsRunning, kAudioObjectPropertyScopeGlobal,
                                    kAudioObjectPropertyElementMain};
    gHost->PropertiesChanged(gHost, dev, 1, &a);
}

static OSStatus ReuseMic_StartIO(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 client) {
    (void)d; (void)client;
    int i = DeviceIndex(dev);
    if (i < 0) return kAudioHardwareBadObjectError;
    int changed;
    pthread_mutex_lock(&gMutex);
    if (gIOCount[0] + gIOCount[1] == 0) {
        gAnchorHostTime = mach_absolute_time();
        atomic_store(&gWriteEnd, INT64_MIN);
    }
    changed = gIOCount[i]++ == 0;
    pthread_mutex_unlock(&gMutex);
    if (changed) NotifyRunning(dev);
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_StopIO(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 client) {
    (void)d; (void)client;
    int i = DeviceIndex(dev);
    if (i < 0) return kAudioHardwareBadObjectError;
    int changed = 0;
    pthread_mutex_lock(&gMutex);
    if (gIOCount[i] > 0) changed = --gIOCount[i] == 0;
    pthread_mutex_unlock(&gMutex);
    if (changed) NotifyRunning(dev);
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_GetZeroTimeStamp(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 client,
                                          Float64 *sampleTime, UInt64 *hostTime, UInt64 *seed) {
    (void)d; (void)client;
    if (!IsDevice(dev)) return kAudioHardwareBadObjectError;
    // Stateless and shared by both devices: the n-th period boundary since the anchor.
    UInt64 now = mach_absolute_time();
    Float64 ticksPerPeriod = gTicksPerFrame * kRingFrames;
    UInt64 n = now > gAnchorHostTime ? (UInt64)((Float64)(now - gAnchorHostTime) / ticksPerPeriod) : 0;
    *sampleTime = (Float64)(n * kRingFrames);
    *hostTime = gAnchorHostTime + (UInt64)((Float64)n * ticksPerPeriod);
    *seed = 1;
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 client,
                                           UInt32 op, Boolean *willDo, Boolean *inPlace) {
    (void)d; (void)client;
    Boolean will = (dev == kObjectID_Mic && op == kAudioServerPlugInIOOperationReadInput) ||
                   (dev == kObjectID_Feed && op == kAudioServerPlugInIOOperationWriteMix);
    if (willDo) *willDo = will;
    if (inPlace) *inPlace = true;
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 client, UInt32 op,
                                          UInt32 frames, const AudioServerPlugInIOCycleInfo *info) {
    (void)d; (void)dev; (void)client; (void)op; (void)frames; (void)info;
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_DoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID dev, AudioObjectID stream,
                                       UInt32 client, UInt32 op, UInt32 frames,
                                       const AudioServerPlugInIOCycleInfo *info, void *main, void *secondary) {
    (void)d; (void)stream; (void)client; (void)secondary;
    if (main == NULL || info == NULL) return kAudioHardwareNoError;
    Float32 *buf = (Float32 *)main;

    if (dev == kObjectID_Feed && op == kAudioServerPlugInIOOperationWriteMix) {
        int64_t start = (int64_t)info->mOutputTime.mSampleTime;
        for (UInt32 i = 0; i < frames; i++) {
            uint64_t slot = (uint64_t)(start + (int64_t)i) % kRingFrames;
            for (UInt32 c = 0; c < kChannels; c++) gRing[slot * kChannels + c] = buf[i * kChannels + c];
        }
        int64_t end = start + (int64_t)frames;
        int64_t prev = atomic_load(&gWriteEnd);
        while (end > prev && !atomic_compare_exchange_weak(&gWriteEnd, &prev, end)) {
        }
    } else if (dev == kObjectID_Mic && op == kAudioServerPlugInIOOperationReadInput) {
        int64_t start = (int64_t)info->mInputTime.mSampleTime;
        int64_t end = atomic_load(&gWriteEnd);
        for (UInt32 i = 0; i < frames; i++) {
            int64_t t = start + (int64_t)i;
            int fresh = end != INT64_MIN && t < end && t >= end - kRingFrames;
            uint64_t slot = (uint64_t)t % kRingFrames;
            for (UInt32 c = 0; c < kChannels; c++)
                buf[i * kChannels + c] = fresh ? gRing[slot * kChannels + c] : 0.0f;
        }
    }
    return kAudioHardwareNoError;
}

static OSStatus ReuseMic_EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 client, UInt32 op,
                                        UInt32 frames, const AudioServerPlugInIOCycleInfo *info) {
    (void)d; (void)dev; (void)client; (void)op; (void)frames; (void)info;
    return kAudioHardwareNoError;
}

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    ReuseMic_QueryInterface,
    ReuseMic_AddRef,
    ReuseMic_Release,
    ReuseMic_Initialize,
    ReuseMic_CreateDevice,
    ReuseMic_DestroyDevice,
    ReuseMic_AddDeviceClient,
    ReuseMic_RemoveDeviceClient,
    ReuseMic_PerformConfigChange,
    ReuseMic_AbortConfigChange,
    ReuseMic_HasProperty,
    ReuseMic_IsPropertySettable,
    ReuseMic_GetPropertyDataSize,
    ReuseMic_GetPropertyData,
    ReuseMic_SetPropertyData,
    ReuseMic_StartIO,
    ReuseMic_StopIO,
    ReuseMic_GetZeroTimeStamp,
    ReuseMic_WillDoIOOperation,
    ReuseMic_BeginIOOperation,
    ReuseMic_DoIOOperation,
    ReuseMic_EndIOOperation,
};
static AudioServerPlugInDriverInterface *gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriverRef = &gInterfacePtr;

static HRESULT ReuseMic_QueryInterface(void *drv, REFIID uuid, LPVOID *out) {
    if (drv != gDriverRef || out == NULL) return kAudioHardwareBadObjectError;
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, uuid);
    if (requested == NULL) return kAudioHardwareIllegalOperationError;
    HRESULT r = E_NOINTERFACE;
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        pthread_mutex_lock(&gMutex);
        gRefCount++;
        pthread_mutex_unlock(&gMutex);
        *out = gDriverRef;
        r = S_OK;
    }
    CFRelease(requested);
    return r;
}

static ULONG ReuseMic_AddRef(void *drv) {
    if (drv != gDriverRef) return 0;
    pthread_mutex_lock(&gMutex);
    ULONG r = ++gRefCount;
    pthread_mutex_unlock(&gMutex);
    return r;
}

static ULONG ReuseMic_Release(void *drv) {
    if (drv != gDriverRef) return 0;
    pthread_mutex_lock(&gMutex);
    if (gRefCount > 0) gRefCount--;
    ULONG r = gRefCount;
    pthread_mutex_unlock(&gMutex);
    return r;
}

// Factory named in Info.plist (CFPlugInFactories).
__attribute__((visibility("default"))) void *ReuseMic_Create(CFAllocatorRef alloc, CFUUIDRef type) {
    (void)alloc;
    if (CFEqual(type, kAudioServerPlugInTypeUUID)) {
        ReuseMic_AddRef(gDriverRef);
        return gDriverRef;
    }
    return NULL;
}
