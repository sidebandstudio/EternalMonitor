// The iPhone Duo simulator folds only from Xcode's Device Hub, which sends a
// private vendor-defined HID event. Nothing public (simctl, devicectl or
// XCUIDevice) can fold it in Xcode 27.1, so this posts the same event from
// inside the simulator and reads the angle back from CoreMotion.
//
// Built two ways: into the UI test runner, so a test can fold mid-stream, and
// as a command-line tool that CI starts with `xcrun simctl spawn` to fold the
// device before a run (-DEM_HINGE_TOOL). Private interfaces, verified on
// Xcode 27.1 with the iOS 27.1 runtime; a check of the reported angle turns
// any change in them into a failure instead of a silent no-op.
//
// Adapted from hinge (https://github.com/artemnovichkov/hinge, abd539d):
//
// MIT License
//
// Copyright (c) 2026 Artem Novichkov
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#import "DuoHinge.h"
#include <dlfcn.h>
#include <mach/mach_time.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct __IOHIDEvent *IOHIDEventRef;
typedef struct __IOHIDEventSystemClient *IOHIDEventSystemClientRef;

// Device Hub's hinge slider and orientation picker: vendor page 0xFF61,
// usage 0x5B, carrying a binary-serialized dictionary with the value.
static const uint32_t kUsagePage = 0xFF61, kUsage = 0x5B;
static const int kClientTypeSimple = 4;
static const CFOptionFlags kSerializeBinary = 1;

@interface NSObject (EMHingeAngle)
+ (BOOL)isAvailable;
- (float)angleDegrees;
- (void)startAngleUpdatesToQueue:(NSOperationQueue *)queue handler:(void (^)(id angle))handler;
- (void)stopAngleUpdates;
@end

static Class AngleManager(void) {
    dlopen("/System/Library/Frameworks/CoreMotion.framework/CoreMotion", RTLD_NOW);
    Class manager = NSClassFromString(@"CMAngleManager");
    return [manager respondsToSelector:@selector(isAvailable)] && [manager isAvailable] ? manager : Nil;
}

static BOOL SendDeviceState(NSString *source, NSString *type, id value, NSString **error) {
    void *iokit = dlopen("/System/Library/Frameworks/IOKit.framework/IOKit", RTLD_NOW);
    IOHIDEventSystemClientRef (*createClient)(CFAllocatorRef, int, CFDictionaryRef) =
        iokit ? dlsym(iokit, "IOHIDEventSystemClientCreateWithType") : NULL;
    void (*dispatch)(IOHIDEventSystemClientRef, IOHIDEventRef) =
        iokit ? dlsym(iokit, "IOHIDEventSystemClientDispatchEvent") : NULL;
    IOHIDEventRef (*createEvent)(CFAllocatorRef, uint64_t, uint32_t, uint32_t, uint32_t, const uint8_t *, CFIndex, uint32_t) =
        iokit ? dlsym(iokit, "IOHIDEventCreateVendorDefinedEvent") : NULL;
    CFDataRef (*serialize)(CFTypeRef, CFOptionFlags) = iokit ? dlsym(iokit, "IOCFSerialize") : NULL;
    if (!createClient || !dispatch || !createEvent || !serialize) {
        if (error) *error = @"This simulator lacks the HID functions Device Hub's hinge uses";
        return NO;
    }
    IOHIDEventSystemClientRef client = createClient(NULL, kClientTypeSimple, NULL);
    if (!client) {
        if (error) *error = @"Could not open the HID event system";
        return NO;
    }
    // The simulator ignores the provider; this is the one Device Hub sends.
    NSDictionary *payload = @{@"provider": @"com.apple.Virtualization.VirtualMachines",
                              @"source": source, @"type": type, @"value": value};
    CFDataRef data = serialize((__bridge CFTypeRef)payload, kSerializeBinary);
    IOHIDEventRef event = data ? createEvent(NULL, mach_absolute_time(), kUsagePage, kUsage, 0,
                                             CFDataGetBytePtr(data), CFDataGetLength(data), 0) : NULL;
    if (event) {
        dispatch(client, event);
        CFRelease(event);
    }
    if (data) CFRelease(data);
    CFRelease(client);
    if (!event && error) *error = @"Could not build the device state event";
    return event != NULL;
}

BOOL EMHingeAvailable(void) {
    return AngleManager() != Nil;
}

BOOL EMSetHingeAngle(double degrees, NSTimeInterval timeout, NSString **error) {
    degrees = fmin(fmax(degrees, 0), 180);
    Class managerClass = AngleManager();
    if (!managerClass) {
        if (error) *error = @"This simulator reports no hinge";
        return NO;
    }
    id manager = [[managerClass alloc] init];
    NSLock *lock = [NSLock new];
    __block double reported = NAN;
    // CoreMotion reports the current angle at once, then every change.
    [manager startAngleUpdatesToQueue:[NSOperationQueue new] handler:^(id angle) {
        [lock lock];
        reported = [angle angleDegrees];
        [lock unlock];
    }];
    BOOL sent = SendDeviceState(@"hinge-slider-control", @"range", @(degrees), error), done = NO;
    double last = NAN;
    CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + timeout;
    // Run this thread's loop meanwhile, as CoreMotion may deliver through it.
    while (sent && !done && CFAbsoluteTimeGetCurrent() < deadline) {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);
        [lock lock];
        last = reported;
        [lock unlock];
        done = fabs(last - degrees) <= 0.5;
    }
    [manager stopAngleUpdates];
    if (sent && !done && error) {
        *error = [NSString stringWithFormat:@"The hinge stayed at %.1f degrees instead of %.1f", last, degrees];
    }
    return done;
}

BOOL EMSetDeviceOrientation(NSString *orientation, NSString **error) {
    if (!AngleManager()) {
        if (error) *error = @"Only the iPhone Duo simulator takes Device Hub's orientation event";
        return NO;
    }
    return SendDeviceState(@"orientation-picker-control", @"enum", orientation, error);
}

NSArray<NSDictionary *> *EMTouchscreens(void) {
    void *iokit = dlopen("/System/Library/Frameworks/IOKit.framework/IOKit", RTLD_NOW);
    IOHIDEventSystemClientRef (*createClient)(CFAllocatorRef, int, CFDictionaryRef) =
        iokit ? dlsym(iokit, "IOHIDEventSystemClientCreateWithType") : NULL;
    CFArrayRef (*copyServices)(IOHIDEventSystemClientRef) = iokit ? dlsym(iokit, "IOHIDEventSystemClientCopyServices") : NULL;
    CFTypeRef (*copyProperty)(CFTypeRef, CFStringRef) = iokit ? dlsym(iokit, "IOHIDServiceClientCopyProperty") : NULL;
    if (!createClient || !copyServices || !copyProperty) return @[];
    IOHIDEventSystemClientRef client = createClient(NULL, 1 /* monitor */, NULL);
    if (!client) return @[];
    NSMutableArray *found = [NSMutableArray array];
    for (id service in CFBridgingRelease(copyServices(client))) {
        NSMutableDictionary *properties = [NSMutableDictionary dictionary];
        for (NSString *key in @[@"PrimaryUsagePage", @"Product", @"displayUUID", @"Built-In"]) {
            id value = CFBridgingRelease(copyProperty((__bridge CFTypeRef)service, (__bridge CFStringRef)key));
            if (value) properties[key] = value;
        }
        // Digitizers: the touchscreens, one per display.
        if ([properties[@"PrimaryUsagePage"] intValue] == 0x0D) [found addObject:properties];
    }
    CFRelease(client);
    return found;
}

#ifdef EM_HINGE_TOOL
int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc != 2) {
            fprintf(stderr, "usage: duo-hinge <degrees, 0 closed to 180 flat>\n");
            return 64;
        }
        NSString *error = nil;
        if (!EMSetHingeAngle(atof(argv[1]), 5, &error)) {
            fprintf(stderr, "duo-hinge: %s\n", error.UTF8String);
            return 1;
        }
        return 0;
    }
}
#endif
