import 'dart:async';
import 'dart:developer' as developer;
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_sound/flutter_sound.dart';
import 'package:record/record.dart';

import 'package:panda_playground/src/rust/api/chat.dart';

void _log(String msg) {
  developer.log(msg, name: 'voice');
  debugPrint('[voice] $msg');
}

/// Voice tab — push-to-talk walkie-talkie over p2p gossip.
class VoiceScreen extends StatefulWidget {
  final String? nodeId;

  const VoiceScreen({super.key, this.nodeId});

  @override
  State<VoiceScreen> createState() => _VoiceScreenState();
}

class _VoiceScreenState extends State<VoiceScreen>
    with AutomaticKeepAliveClientMixin {
  final AudioRecorder _recorder = AudioRecorder();
  final FlutterSoundPlayer _player = FlutterSoundPlayer();

  bool _sessionReady = false;
  bool _transmitting = false;
  bool _receiving = false;
  String? _error;

  StreamSubscription<Uint8List>? _voiceSubscription;
  StreamSubscription<Uint8List>? _micSubscription;

  @override
  bool get wantKeepAlive => true;

  @override
  void initState() {
    super.initState();
    _initVoice();
  }

  Future<void> _initVoice() async {
    try {
      // Initialize Opus encoder/decoder on Rust side.
      await startVoiceSession();
      _log('opus session initialized');

      // Open the player and start streaming mode for PCM playback.
      await _player.openPlayer();
      await _player.startPlayerFromStream(
        codec: Codec.pcm16,
        interleaved: true,
        numChannels: 1,
        sampleRate: 16000,
        // 1280 bytes = 2x 20ms frames. The previous 4096 (~128ms) consumed
        // half the real-time-voice latency budget on playback buffering alone.
        bufferSize: 1280,
      );
      _log('player opened in streaming mode');

      setState(() => _sessionReady = true);

      // Subscribe to incoming voice frames.
      _voiceSubscription = subscribeVoice().listen(
        _onVoiceFrame,
        onError: (e) => _log('voice stream error: $e'),
        onDone: () => _log('voice stream closed'),
      );
      _log('voice subscription active');
    } catch (e) {
      _log('init error: $e');
      setState(() => _error = e.toString());
    }
  }

  Timer? _receiveTimer;

  /// Called for each decoded PCM frame received from a peer.
  void _onVoiceFrame(Uint8List pcmBytes) {
    if (!_receiving) {
      setState(() => _receiving = true);
    }
    // Reset the silence timer — mark as not receiving after 200ms of no frames.
    _receiveTimer?.cancel();
    _receiveTimer = Timer(const Duration(milliseconds: 200), () {
      if (mounted) setState(() => _receiving = false);
    });

    // Feed raw PCM bytes to the player.
    _player.feedUint8FromStream(pcmBytes);
  }

  /// Start transmitting — capture mic and send frames.
  Future<void> _startTransmit() async {
    if (!_sessionReady || _transmitting) return;

    final hasPermission = await _recorder.hasPermission();
    if (!hasPermission) {
      _log('microphone permission denied');
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(
            content: Text(
              'Microphone permission is required for voice chat. '
              'Enable it in Settings.',
            ),
            behavior: SnackBarBehavior.floating,
          ),
        );
      }
      return;
    }

    setState(() => _transmitting = true);
    _log('PTT start');

    // Start recording mic as 16kHz mono 16-bit PCM stream.
    final stream = await _recorder.startStream(
      const RecordConfig(
        encoder: AudioEncoder.pcm16bits,
        sampleRate: 16000,
        numChannels: 1,
        autoGain: true,
        echoCancel: true,
        noiseSuppress: true,
      ),
    );

    // Buffer incoming PCM and send in 640-byte frames (320 samples = 20ms).
    final buffer = BytesBuilder(copy: false);
    _micSubscription = stream.listen((chunk) {
      buffer.add(chunk);
      while (buffer.length >= 640) {
        final bytes = buffer.takeBytes();
        // Take exactly 640 bytes, put remainder back.
        final frame = bytes.sublist(0, 640);
        if (bytes.length > 640) {
          buffer.add(bytes.sublist(640));
        }
        sendVoiceFrame(pcmBytes: frame).catchError((e) {
          _log('send voice error: $e');
        });
      }
    });

    _log('mic stream active');
  }

  /// Stop transmitting.
  Future<void> _stopTransmit() async {
    if (!_transmitting) return;

    setState(() => _transmitting = false);
    _log('PTT stop');

    await _micSubscription?.cancel();
    _micSubscription = null;
    await _recorder.stop();
  }

  @override
  void dispose() {
    _receiveTimer?.cancel();
    _voiceSubscription?.cancel();
    _micSubscription?.cancel();
    _recorder.dispose();
    _player.closePlayer();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    super.build(context);
    final theme = Theme.of(context);

    if (_error != null) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Text(
            'Voice error: $_error',
            style: TextStyle(color: theme.colorScheme.error),
          ),
        ),
      );
    }

    if (!_sessionReady) {
      return const Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            CircularProgressIndicator(),
            SizedBox(height: 16),
            Text('Initializing voice...'),
          ],
        ),
      );
    }

    return Column(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        // Receiving indicator
        AnimatedOpacity(
          opacity: _receiving ? 1.0 : 0.0,
          duration: const Duration(milliseconds: 150),
          child: Padding(
            padding: const EdgeInsets.only(bottom: 32),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(Icons.volume_up, color: theme.colorScheme.secondary),
                const SizedBox(width: 8),
                Text(
                  'Receiving...',
                  style: TextStyle(color: theme.colorScheme.secondary),
                ),
              ],
            ),
          ),
        ),

        // Push-to-talk button
        GestureDetector(
          onLongPressStart: (_) => _startTransmit(),
          onLongPressEnd: (_) => _stopTransmit(),
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 150),
            width: _transmitting ? 160 : 140,
            height: _transmitting ? 160 : 140,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: _transmitting
                  ? theme.colorScheme.error
                  : theme.colorScheme.primaryContainer,
              boxShadow: _transmitting
                  ? [
                      BoxShadow(
                        color: theme.colorScheme.error.withValues(alpha: 0.4),
                        blurRadius: 24,
                        spreadRadius: 8,
                      )
                    ]
                  : [],
            ),
            child: Icon(
              _transmitting ? Icons.mic : Icons.mic_none,
              size: 64,
              color: _transmitting
                  ? theme.colorScheme.onError
                  : theme.colorScheme.onPrimaryContainer,
            ),
          ),
        ),

        const SizedBox(height: 24),
        Text(
          _transmitting ? 'Transmitting...' : 'Hold to talk',
          style: theme.textTheme.titleMedium?.copyWith(
            color: _transmitting
                ? theme.colorScheme.error
                : theme.colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }
}
