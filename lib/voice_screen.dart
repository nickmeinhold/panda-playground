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

/// Push-to-talk transmit lifecycle.
///
/// The transitions are: `idle → starting → transmitting → stopping → idle`,
/// plus `starting → idle` on failure (e.g. recorder error after permission
/// was granted). Modelling these explicitly avoids the "stuck in
/// Transmitting" bug that a pair of booleans can't represent — every state
/// has exactly one valid set of outgoing transitions.
enum _TransmitState { idle, starting, transmitting, stopping }

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
  final BytesBuilder _frameBuffer = BytesBuilder(copy: false);

  bool _sessionReady = false;
  bool _receiving = false;
  bool _voiceStreamHealthy = true;
  bool _reconnecting = false;
  bool _permissionWarningShown = false;
  _TransmitState _transmit = _TransmitState.idle;
  String? _error;

  StreamSubscription<Uint8List>? _voiceSubscription;
  StreamSubscription<Uint8List>? _micSubscription;
  Timer? _receiveTimer;

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

      _subscribeToVoice();
    } catch (e) {
      _log('init error: $e');
      setState(() => _error = e.toString());
    }
  }

  /// Wire up the voice-receive subscription. Stream errors / completion are
  /// surfaced into UI via `_voiceStreamHealthy` so a silent gossip-stream
  /// termination doesn't look like a working voice tab.
  ///
  /// Used at first init and again on user-driven reconnect (tap-to-retry).
  void _subscribeToVoice() {
    _voiceSubscription = subscribeVoice().listen(
      _onVoiceFrame,
      onError: (Object e) {
        _log('voice stream error: $e');
        if (mounted) setState(() => _voiceStreamHealthy = false);
      },
      onDone: () {
        _log('voice stream closed');
        if (mounted) setState(() => _voiceStreamHealthy = false);
      },
    );
    _log('voice subscription active');
  }

  /// Tear down the dead subscription and start a fresh one. The Rust-side
  /// task that fed the previous subscription has already exited (that's
  /// why the Dart stream surfaced onError/onDone in the first place), so
  /// this is cheap — a new tokio task, a new mpsc receiver against the same
  /// underlying gossip stream.
  Future<void> _tryReconnect() async {
    if (_reconnecting) return;
    setState(() => _reconnecting = true);
    _log('reconnect attempt');

    try {
      await _voiceSubscription?.cancel();
    } catch (e) {
      _log('cancel dead subscription failed (expected): $e');
    }
    _voiceSubscription = null;

    try {
      _subscribeToVoice();
      if (mounted) {
        setState(() {
          _voiceStreamHealthy = true;
          _reconnecting = false;
        });
      }
      _log('reconnect succeeded');
    } catch (e) {
      _log('reconnect failed: $e');
      if (mounted) {
        setState(() => _reconnecting = false);
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text('Reconnect failed: $e'),
            behavior: SnackBarBehavior.floating,
          ),
        );
      }
    }
  }

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

  /// Buffer mic-stream chunks and emit exact 640-byte (320-sample) frames.
  void _onMicChunk(Uint8List chunk) {
    _frameBuffer.add(chunk);
    while (_frameBuffer.length >= 640) {
      final bytes = _frameBuffer.takeBytes();
      final frame = bytes.sublist(0, 640);
      if (bytes.length > 640) {
        _frameBuffer.add(bytes.sublist(640));
      }
      sendVoiceFrame(pcmBytes: frame).catchError((Object e) {
        _log('send voice error: $e');
      });
    }
  }

  /// Start transmitting — capture mic and send frames.
  Future<void> _startTransmit() async {
    if (_transmit != _TransmitState.idle || !_sessionReady) return;

    final hasPermission = await _recorder.hasPermission();
    if (!hasPermission) {
      _log('microphone permission denied');
      if (mounted && !_permissionWarningShown) {
        _permissionWarningShown = true;
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

    setState(() => _transmit = _TransmitState.starting);
    _log('PTT starting');

    try {
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
      _micSubscription = stream.listen(_onMicChunk);
      if (!mounted) return;
      setState(() => _transmit = _TransmitState.transmitting);
      _log('mic stream active');
    } catch (e) {
      _log('startStream failed: $e');
      // Roll back to idle so the UI doesn't get stuck showing "Transmitting".
      if (mounted) {
        setState(() => _transmit = _TransmitState.idle);
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text('Could not start microphone: $e'),
            behavior: SnackBarBehavior.floating,
          ),
        );
      }
    }
  }

  /// Stop transmitting.
  ///
  /// Defensive: both `_micSubscription.cancel()` and `_recorder.stop()` are
  /// wrapped in try/catch so a partial-start failure (e.g. mic stream never
  /// fully attached) can't leave the UI stuck in `stopping`.
  Future<void> _stopTransmit() async {
    if (_transmit != _TransmitState.transmitting &&
        _transmit != _TransmitState.starting) {
      return;
    }

    setState(() => _transmit = _TransmitState.stopping);
    _log('PTT stopping');

    try {
      await _micSubscription?.cancel();
    } catch (e) {
      _log('mic subscription cancel failed: $e');
    }
    _micSubscription = null;

    try {
      await _recorder.stop();
    } catch (e) {
      _log('recorder stop failed: $e');
    }

    // Drop any partial frame left in the buffer so the next PTT session
    // doesn't begin with stale samples.
    _frameBuffer.clear();

    if (mounted) {
      setState(() => _transmit = _TransmitState.idle);
    }
    _log('PTT stopped');
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

  /// True while the mic button should look "hot" (active glow / red colour).
  /// Covers both the mid-start transient and full transmission so the user
  /// gets immediate visual feedback when holding the button.
  bool get _micActive =>
      _transmit == _TransmitState.starting ||
      _transmit == _TransmitState.transmitting;

  String get _pttLabel {
    switch (_transmit) {
      case _TransmitState.idle:
        return 'Hold to talk';
      case _TransmitState.starting:
        return 'Starting...';
      case _TransmitState.transmitting:
        return 'Transmitting...';
      case _TransmitState.stopping:
        return 'Stopping...';
    }
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
        // Disconnected banner — voice gossip stream ended or errored. The
        // mic still works (transmits won't fail) but no incoming voice
        // will arrive, so this needs to be visible. Tap to retry.
        if (!_voiceStreamHealthy)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 24)
                .copyWith(bottom: 16),
            child: Material(
              color: theme.colorScheme.errorContainer,
              borderRadius: BorderRadius.circular(8),
              child: InkWell(
                borderRadius: BorderRadius.circular(8),
                onTap: _reconnecting ? null : _tryReconnect,
                child: Padding(
                  padding: const EdgeInsets.all(12),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (_reconnecting)
                        SizedBox(
                          width: 20,
                          height: 20,
                          child: CircularProgressIndicator(
                            strokeWidth: 2,
                            color: theme.colorScheme.onErrorContainer,
                          ),
                        )
                      else
                        Icon(
                          Icons.cloud_off,
                          color: theme.colorScheme.onErrorContainer,
                        ),
                      const SizedBox(width: 8),
                      Flexible(
                        child: Text(
                          _reconnecting
                              ? 'Reconnecting...'
                              : 'Voice stream disconnected — tap to reconnect.',
                          style: TextStyle(
                            color: theme.colorScheme.onErrorContainer,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),

        // Receiving indicator
        AnimatedOpacity(
          opacity: _receiving && _voiceStreamHealthy ? 1.0 : 0.0,
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
            width: _micActive ? 160 : 140,
            height: _micActive ? 160 : 140,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: _micActive
                  ? theme.colorScheme.error
                  : theme.colorScheme.primaryContainer,
              boxShadow: _micActive
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
              _micActive ? Icons.mic : Icons.mic_none,
              size: 64,
              color: _micActive
                  ? theme.colorScheme.onError
                  : theme.colorScheme.onPrimaryContainer,
            ),
          ),
        ),

        const SizedBox(height: 24),
        Text(
          _pttLabel,
          style: theme.textTheme.titleMedium?.copyWith(
            color: _micActive
                ? theme.colorScheme.error
                : theme.colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }
}
