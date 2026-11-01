// Loads the built plug-in binary in-process with a fake host and checks properties + loopback.
// Usage: test_driver build/ReuseMic.driver/Contents/MacOS/ReuseMic

#include <CoreAudio/AudioServerPlugIn.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define CHECK(c)                                                         \
    do {                                                                 \
        if (!(c)) {                                                      \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
            exit(1);                                                     \
        }                                                                \
    } while (0)

static int gChanged = 0;
static OSStatus PropertiesChanged(AudioServerPlugInHostRef h, AudioObjectID o, UInt32 n,
                                  const AudioObjectPropertyAddress *a) {
    (void)h; (void)o; (void)n; (void)a;
    gChanged++;
    return 0;
}

static AudioServerPlugInHostInterface gHost = {0};

static AudioObjectPropertyAddress A(AudioObjectPropertySelector s, AudioObjectPropertyScope sc) {
    AudioObjectPropertyAddress a = {s, sc, kAudioObjectPropertyElementMain};
    return a;
}

int main(int argc, char **argv) {
    CHECK(argc == 2);
    void *lib = dlopen(argv[1], RTLD_NOW);
    if (!lib) fprintf(stderr, "%s\n", dlerror());
    CHECK(lib);
    void *(*create)(CFAllocatorRef, CFUUIDRef) = dlsym(lib, "ReuseMic_Create");
    CHECK(create);
    AudioServerPlugInDriverRef drv = create(NULL, kAudioServerPlugInTypeUUID);
    CHECK(drv);
    AudioServerPlugInDriverInterface *d = *drv;

    void *iface = NULL;
    CHECK(d->QueryInterface(drv, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), &iface) == 0);
    CHECK(iface == drv);

    gHost.PropertiesChanged = PropertiesChanged;
    CHECK(d->Initialize(drv, &gHost) == 0);

    // Plug-in device list.
    AudioObjectPropertyAddress a = A(kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal);
    UInt32 size = 0;
    CHECK(d->GetPropertyDataSize(drv, 1, 0, &a, 0, NULL, &size) == 0 && size == 8);
    AudioObjectID devs[4];
    CHECK(d->GetPropertyData(drv, 1, 0, &a, 0, NULL, sizeof(devs), &size, devs) == 0);
    CHECK(devs[0] == 2 && devs[1] == 4);

    // UID translation.
    a = A(kAudioPlugInPropertyTranslateUIDToDevice, kAudioObjectPropertyScopeGlobal);
    CHECK(d->HasProperty(drv, 1, 0, &a));
    CFStringRef uid = CFSTR("ReuseMic_Feed_UID");
    AudioObjectID found = 0;
    CHECK(d->GetPropertyData(drv, 1, 0, &a, sizeof(uid), &uid, sizeof(found), &size, &found) == 0 && found == 4);
    uid = CFSTR("nope");
    CHECK(d->GetPropertyData(drv, 1, 0, &a, sizeof(uid), &uid, sizeof(found), &size, &found) == 0 && found == 0);

    // Device names and streams.
    a = A(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal);
    CFStringRef name = NULL;
    CHECK(d->GetPropertyData(drv, 2, 0, &a, 0, NULL, sizeof(name), &size, &name) == 0);
    CHECK(CFStringCompare(name, CFSTR("ReuseMic"), 0) == kCFCompareEqualTo);
    a = A(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput);
    AudioObjectID streams[2];
    CHECK(d->GetPropertyData(drv, 2, 0, &a, 0, NULL, sizeof(streams), &size, streams) == 0 && size == 4 &&
          streams[0] == 3);
    a = A(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput);
    CHECK(d->GetPropertyData(drv, 2, 0, &a, 0, NULL, sizeof(streams), &size, streams) == 0 && size == 0);
    a = A(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput);
    CHECK(d->GetPropertyData(drv, 4, 0, &a, 0, NULL, sizeof(streams), &size, streams) == 0 && size == 4 &&
          streams[0] == 5);
    a = A(kAudioDevicePropertyIsHidden, kAudioObjectPropertyScopeGlobal);
    UInt32 hidden = 9;
    CHECK(d->GetPropertyData(drv, 4, 0, &a, 0, NULL, 4, &size, &hidden) == 0 && hidden == 1);
    CHECK(d->GetPropertyData(drv, 2, 0, &a, 0, NULL, 4, &size, &hidden) == 0 && hidden == 0);

    // Stream format.
    a = A(kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal);
    AudioStreamBasicDescription f;
    CHECK(d->GetPropertyData(drv, 3, 0, &a, 0, NULL, sizeof(f), &size, &f) == 0);
    CHECK(f.mSampleRate == 48000 && f.mChannelsPerFrame == 1 && f.mBitsPerChannel == 32);
    a = A(kAudioStreamPropertyDirection, kAudioObjectPropertyScopeGlobal);
    UInt32 dir = 9;
    CHECK(d->GetPropertyData(drv, 3, 0, &a, 0, NULL, 4, &size, &dir) == 0 && dir == 1);
    CHECK(d->GetPropertyData(drv, 5, 0, &a, 0, NULL, 4, &size, &dir) == 0 && dir == 0);

    // Unknown property / object.
    a = A('zzzz', kAudioObjectPropertyScopeGlobal);
    CHECK(!d->HasProperty(drv, 2, 0, &a));
    a = A(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal);
    CHECK(!d->HasProperty(drv, 99, 0, &a));

    // Loopback: write on the feed at sample time T, read on the mic at T.
    Boolean will, inplace;
    CHECK(d->WillDoIOOperation(drv, 4, 0, kAudioServerPlugInIOOperationWriteMix, &will, &inplace) == 0 && will);
    CHECK(d->WillDoIOOperation(drv, 2, 0, kAudioServerPlugInIOOperationReadInput, &will, &inplace) == 0 && will);
    CHECK(d->WillDoIOOperation(drv, 2, 0, kAudioServerPlugInIOOperationWriteMix, &will, &inplace) == 0 && !will);
    CHECK(d->StartIO(drv, 2, 0) == 0);
    CHECK(d->StartIO(drv, 4, 0) == 0);
    CHECK(gChanged == 2);

    Float64 st0, st1;
    UInt64 ht0, ht1, seed;
    CHECK(d->GetZeroTimeStamp(drv, 2, 0, &st0, &ht0, &seed) == 0);
    CHECK(d->GetZeroTimeStamp(drv, 4, 0, &st1, &ht1, &seed) == 0);
    CHECK(st0 == st1 && ht0 == ht1);

    enum { N = 512 };
    float wbuf[N], rbuf[N];
    AudioServerPlugInIOCycleInfo info;
    memset(&info, 0, sizeof(info));

    // Before any write the mic is silent.
    for (int i = 0; i < N; i++) rbuf[i] = 1;
    info.mInputTime.mSampleTime = 1000;
    CHECK(d->DoIOOperation(drv, 2, 3, 0, kAudioServerPlugInIOOperationReadInput, N, &info, rbuf, NULL) == 0);
    for (int i = 0; i < N; i++) CHECK(rbuf[i] == 0);

    for (int i = 0; i < N; i++) wbuf[i] = (float)i + 1;
    info.mOutputTime.mSampleTime = 20000; // wraps the 16384 ring
    CHECK(d->DoIOOperation(drv, 4, 5, 0, kAudioServerPlugInIOOperationWriteMix, N, &info, wbuf, NULL) == 0);
    info.mInputTime.mSampleTime = 20000 + 100;
    CHECK(d->DoIOOperation(drv, 2, 3, 0, kAudioServerPlugInIOOperationReadInput, N, &info, rbuf, NULL) == 0);
    for (int i = 0; i < N - 100; i++) CHECK(rbuf[i] == wbuf[i + 100]);
    for (int i = N - 100; i < N; i++) CHECK(rbuf[i] == 0); // not written yet

    // Stale data (older than the ring) reads as silence.
    info.mInputTime.mSampleTime = 20000 - 16384 - N;
    CHECK(d->DoIOOperation(drv, 2, 3, 0, kAudioServerPlugInIOOperationReadInput, N, &info, rbuf, NULL) == 0);
    for (int i = 0; i < N; i++) CHECK(rbuf[i] == 0);

    CHECK(d->StopIO(drv, 4, 0) == 0);
    CHECK(d->StopIO(drv, 2, 0) == 0);
    CHECK(gChanged == 4);

    // Restarting resets the write position.
    CHECK(d->StartIO(drv, 2, 0) == 0);
    info.mInputTime.mSampleTime = 20000;
    CHECK(d->DoIOOperation(drv, 2, 3, 0, kAudioServerPlugInIOOperationReadInput, N, &info, rbuf, NULL) == 0);
    for (int i = 0; i < N; i++) CHECK(rbuf[i] == 0);
    CHECK(d->StopIO(drv, 2, 0) == 0);

    printf("driver tests passed\n");
    return 0;
}
