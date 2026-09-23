import unittest
from real_checks import check_probe, check_stream


class RealChecksTests(unittest.TestCase):
    def result(self):
        return dict(status='PASS', measured_seconds=20)

    def log(self, encoder='h264_nvenc', bitrate=40000000):
        return f'Desktop duplication active\nEncoder opened bitrate={bitrate} encoder="{encoder}"\n'

    def test_encoder_override_is_not_proof_that_hardware_opened(self):
        result = check_stream(self.result(), 'Encoder override honoured encoder=h264_nvenc\n' +
                              self.log('libx264'), 'h264_nvenc')
        self.assertEqual(result['status'], 'FAIL')
        self.assertIn('libx264', result['errors'][0])

    def test_hevc_may_open_h264_before_negotiation_but_must_finish_in_hevc(self):
        log = self.log() + self.log('hevc_nvenc')
        self.assertEqual(check_stream(self.result(), log, 'hevc_nvenc')['status'], 'PASS')
        self.assertEqual(check_stream(self.result(), log + self.log(), 'hevc_nvenc')['status'], 'FAIL')

    def test_burst_rejects_abr_reopen_below_forty_mbps(self):
        result = check_stream(self.result(), self.log() + self.log(bitrate=15000000),
                              'h264_nvenc', bitrate=40000000)
        self.assertEqual(result['status'], 'FAIL')

    def test_repairs_and_keyframe_storm_remain_separate_requirements(self):
        log = self.log() + 'E2E_HOST_STATS retransmits=123\n'
        self.assertEqual(check_stream(self.result(), log, 'h264_nvenc', repairs=True)['status'], 'PASS')
        self.assertEqual(check_stream(self.result(), self.log(), 'h264_nvenc', repairs=True)['status'], 'FAIL')
        self.assertEqual(check_stream(self.result(), log + 'Keyframe request received\n' * 3,
                                      'h264_nvenc', repairs=True)['status'], 'FAIL')

    def test_audio_requires_wasapi_and_a_growing_quiet_packet_count(self):
        log = self.log() + 'WASAPI default endpoint\nAudio stream stats quiet_packets=3\n'
        self.assertEqual(check_stream(self.result(), log, 'h264_nvenc', audio=True)['status'], 'FAIL')
        log += 'Audio stream stats quiet_packets=16\n'
        self.assertEqual(check_stream(self.result(), log, 'h264_nvenc', audio=True)['status'], 'PASS')

    def test_high_refresh_requires_actual_120_fps_encoder(self):
        from real_checks import check_high_refresh
        with self.assertRaises(ValueError):
            check_high_refresh(self.result(), self.log() + 'Encoder opened fps=60\n')
        result = check_high_refresh(self.result(), self.log() + 'Encoder opened fps=120\n')
        self.assertEqual(result['target_fps'], 120)


class BgraChecksTests(unittest.TestCase):
    def fixture(self):
        return (dict(status='PASS', measured_seconds=601),
                'Encoder opened encoder="h264_nvenc" input=BGRA\n',
                dict(width=2420, height=1668, quadrants_rgb=[[210,40,50],[35,180,80],[40,70,210],[180,180,180]]))

    def test_matching_color_and_ten_minutes_pass(self):
        from real_checks import check_bgra
        result, log, colors = self.fixture()
        self.assertEqual(check_bgra(result, log, colors, colors)['color_mean_channel_errors'], [0,0,0,0])

    def test_channel_swap_short_run_and_yuv_fallback_fail(self):
        from real_checks import check_bgra
        import copy
        for defect in ('swap', 'duration', 'fallback', 'error', 'missing', 'geometry'):
            with self.subTest(defect=defect):
                result, log, colors = self.fixture()
                reference = copy.deepcopy(colors)
                if defect == 'swap': colors['quadrants_rgb'][0].reverse()
                elif defect == 'duration': result['measured_seconds'] = 599
                elif defect == 'fallback': log += 'Encoder opened input=YUV420P\n'
                elif defect == 'error': log += 'ERROR Encoder failed\n'
                elif defect == 'missing': colors['quadrants_rgb'] = []
                else: colors['width'] += 1
                with self.assertRaises(ValueError): check_bgra(result, log, colors, reference)


class ProbeChecksTests(unittest.TestCase):
    def fixture(self):
        expected = dict(width=1920, height=1080, clicks=[[960,540],[20,20],[1899,20],[20,1059],[1899,1059]],
                        drag_start=[384,432], drag_end=[1536,648], right_click=[1152,432])
        events = [dict(event='Ready',x=0,y=0,width=1920,height=1080), dict(event='Armed')]
        for x,y in expected['clicks']:
            events += [dict(event=kind,x=x,y=y,button='Left') for kind in ['MouseDown','MouseUp']]
        events += [dict(event='MouseDown',x=384,y=432,button='Left')]
        events += [dict(event='MouseMove',x=384+i*100,y=432+i*20,button='Left') for i in range(1,7)]
        events += [dict(event='MouseUp',x=1536,y=648,button='Left')]
        events += [dict(event=kind,x=1152,y=432,button='Right') for kind in ['MouseDown','MouseUp']]
        events += [dict(event='MouseWheel',delta=-90) for _ in range(2)]
        events += [dict(event='KeyPress',char=char) for char in 'Hi!\r']
        events += [dict(event=kind,keycode=13,scan=28) for kind in ['KeyDown','KeyUp']]
        return events,expected

    def test_observed_full_gesture_sequence_passes(self):
        self.assertEqual(check_probe(*self.fixture())['input_mapping_error_px'],0)

    def test_four_pixel_mapping_error_fails(self):
        events,expected = self.fixture()
        next(e for e in events if e['event'] == 'MouseDown')['x'] += 4
        with self.assertRaisesRegex(ValueError,'4.0px'):
            check_probe(events,expected)

    def test_reversed_scroll_focus_loss_and_missing_text_fail(self):
        for defect in ['scroll','focus','text','drag']:
            with self.subTest(defect=defect):
                events,expected = self.fixture()
                if defect == 'scroll':
                    for event in events:
                        if event['event'] == 'MouseWheel': event['delta'] *= -1
                elif defect == 'focus': events += [dict(event='Deactivated')]
                elif defect == 'text': events = [e for e in events if e.get('char') != '!']
                else: events = [e for e in events if e['event'] != 'MouseMove']
                with self.assertRaises(ValueError): check_probe(events,expected)

    def test_startup_focus_change_is_allowed_only_before_arming(self):
        events, expected = self.fixture()
        events.insert(1, dict(event='Deactivated'))
        self.assertEqual(check_probe(events, expected)['input_mapping_error_px'], 0)
        events.append(dict(event='Deactivated'))
        with self.assertRaisesRegex(ValueError, 'lost foreground'):
            check_probe(events, expected)

    def test_missing_arm_or_input_before_arming_fails(self):
        events, expected = self.fixture()
        with self.assertRaisesRegex(ValueError, 'exactly once'):
            check_probe([e for e in events if e['event'] != 'Armed'], expected)
        events.insert(1, dict(event='KeyPress', char='H'))
        with self.assertRaisesRegex(ValueError, 'before it was armed'):
            check_probe(events, expected)

    def test_only_one_explicit_center_click_can_activate_the_probe(self):
        events, expected = self.fixture()
        setup = [dict(event='FocusClick', x=960, y=540)] + [dict(event=kind, button='Left', x=960, y=540) for kind in ('MouseDown', 'MouseUp')]
        events[1:1] = setup
        self.assertEqual(check_probe(events, expected)['input_mapping_error_px'], 0)
        setup[1]['x'] += 1
        with self.assertRaisesRegex(ValueError, 'bounded center click'):
            check_probe(events, expected)


class AdvertisedDisplayTests(unittest.TestCase):
    def state(self, hz):
        return {'disabled': False, 'settings_xml': f'<vdd_settings><resolutions><resolution><width>2420</width><height>1668</height><refresh_rate>{hz}</refresh_rate></resolution></resolutions></vdd_settings>'}

    def test_sixty_and_120_hz_modes_follow_advertisement(self):
        from real_checks import check_vdd_mode
        for hz in (60, 120):
            for width, height in ((2420, 1668), (1668, 2420)):
                self.assertEqual(check_vdd_mode(self.state(hz), f'E2E_HELLO w={width} h={height} refresh_hz={hz}'), (2420, 1668, hz))

    def test_lower_mode_and_missing_advertisement_are_rejected(self):
        from real_checks import check_vdd_mode
        for milestone in ('', 'E2E_HELLO w=2420 h=1668 refresh_hz=120'):
            with self.assertRaises(ValueError):
                check_vdd_mode(self.state(60), milestone)


if __name__ == '__main__':
    unittest.main()
