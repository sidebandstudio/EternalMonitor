// Bridging header for the Mac probe: the private CGVirtualDisplay classes
// (declared as Chromium's ui/display/mac/test/virtual_display_mac_util.mm
// does) and the C ABI of ../ffi (the host's direct VideoToolbox encoder).
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <stdbool.h>
#include <stdint.h>

NS_ASSUME_NONNULL_BEGIN

@class CGVirtualDisplay;

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsHigh;
@property(nonatomic) unsigned int maxPixelsWide;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) unsigned int serialNum;
@property(nonatomic) unsigned int productID;
@property(nonatomic) unsigned int vendorID;
@property(copy, nonatomic, nullable) void (^terminationHandler)(id, CGVirtualDisplay *);
@end

@interface CGVirtualDisplayMode : NSObject
@property(readonly, nonatomic) unsigned int width;
@property(readonly, nonatomic) unsigned int height;
@property(readonly, nonatomic) double refreshRate;
- (instancetype)initWithWidth:(unsigned int)width
                       height:(unsigned int)height
                  refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray<CGVirtualDisplayMode *> *modes;
@property(nonatomic) unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
@property(readonly, nonatomic) CGDirectDisplayID displayID;
- (nullable instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@end

NS_ASSUME_NONNULL_END

// ../ffi/src/lib.rs
typedef struct ProbeEncoder ProbeEncoder;
typedef struct { uint32_t encode_fps; uint8_t level_idc; bool high_tier; uint32_t max_bitrate_bps; } EmPlan;
typedef struct { const uint8_t *data; size_t len; uint64_t frame_number; uint64_t encode_us; bool keyframe; } EmFrame;
typedef struct { bool valid; uint8_t profile_idc; bool high_tier; uint8_t level_idc; } EmLevel;

ProbeEncoder *_Nullable em_encoder_create(bool hevc, uint32_t width, uint32_t height, uint32_t fps,
                                          uint32_t bitrate_bps, bool experimental, EmPlan *_Nullable plan,
                                          char *_Nullable error, size_t error_capacity);
int32_t em_encoder_encode(ProbeEncoder *_Nonnull encoder, void *_Nonnull pixel_buffer, uint64_t frame_number,
                          bool force_keyframe);
const char *_Nonnull em_encoder_error(const ProbeEncoder *_Nonnull encoder);
bool em_encoder_next(ProbeEncoder *_Nonnull encoder, EmFrame *_Nonnull frame);
uint32_t em_encoder_in_flight(const ProbeEncoder *_Nonnull encoder);
EmLevel em_encoder_emitted(const ProbeEncoder *_Nonnull encoder);
int32_t em_encoder_flush(ProbeEncoder *_Nonnull encoder);
void em_encoder_destroy(ProbeEncoder *_Nullable encoder);
