import 'dart:async';
import 'dart:developer' as developer;
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:panda_playground/src/rust/api/chat.dart';

void _log(String msg) {
  developer.log(msg, name: 'sketch');
  debugPrint('[sketch] $msg');
}

/// A stroke is a list of points with a color, drawn by a specific peer.
class Stroke {
  final String senderId;
  final Color color;
  final List<Offset> points;

  Stroke({required this.senderId, required this.color, required this.points});

  /// Encode as "color_hex:x1,y1;x2,y2;..." (sender prepended by Rust API).
  String encode(Size canvasSize) {
    final colorHex =
        color.toARGB32().toRadixString(16).padLeft(8, '0').substring(2);
    final pts = points
        .map((p) =>
            '${(p.dx / canvasSize.width).toStringAsFixed(4)},${(p.dy / canvasSize.height).toStringAsFixed(4)}')
        .join(';');
    return '$colorHex:$pts';
  }

  /// Decode from "sender_id:color_hex:x1,y1;x2,y2;..."
  static Stroke? decode(String raw, Size canvasSize) {
    // Format: sender_id:color_hex:x1,y1;x2,y2;...
    final firstColon = raw.indexOf(':');
    if (firstColon == -1) return null;
    final senderId = raw.substring(0, firstColon);

    final rest = raw.substring(firstColon + 1);
    final secondColon = rest.indexOf(':');
    if (secondColon == -1) return null;

    final colorHex = rest.substring(0, secondColon);
    final pointsStr = rest.substring(secondColon + 1);

    final color = Color(int.parse('ff$colorHex', radix: 16));

    final points = <Offset>[];
    for (final pt in pointsStr.split(';')) {
      final parts = pt.split(',');
      if (parts.length != 2) continue;
      final x = double.tryParse(parts[0]);
      final y = double.tryParse(parts[1]);
      if (x == null || y == null) continue;
      points.add(Offset(x * canvasSize.width, y * canvasSize.height));
    }

    if (points.isEmpty) return null;
    return Stroke(senderId: senderId, color: color, points: points);
  }
}

class SketchScreen extends StatefulWidget {
  final String? nodeId;

  const SketchScreen({super.key, this.nodeId});

  @override
  State<SketchScreen> createState() => _SketchScreenState();
}

class _SketchScreenState extends State<SketchScreen> {
  final List<Stroke> _strokes = [];
  List<Offset> _currentPoints = [];
  StreamSubscription<String>? _subscription;
  Color _myColor = _randomColor();
  final GlobalKey _canvasKey = GlobalKey();

  static final List<Color> _colors = [
    Colors.red.shade400,
    Colors.blue.shade400,
    Colors.green.shade400,
    Colors.orange.shade400,
    Colors.purple.shade400,
    Colors.teal.shade400,
    Colors.pink.shade400,
    Colors.cyan.shade400,
  ];

  static Color _randomColor() => _colors[math.Random().nextInt(_colors.length)];

  @override
  void initState() {
    super.initState();
    _subscribe();
  }

  void _subscribe() {
    _log('subscribing to sketch stream...');
    _subscription = subscribeSketch().listen(
      (raw) {
        final size = _canvasSize;
        if (size == null) return;

        final stroke = Stroke.decode(raw, size);
        if (stroke == null) return;

        // Skip our own strokes
        if (stroke.senderId == widget.nodeId) return;

        _log('received stroke from ${stroke.senderId} (${stroke.points.length} points)');
        setState(() => _strokes.add(stroke));
      },
      onError: (e) => _log('sketch stream error: $e'),
      onDone: () => _log('sketch stream closed'),
    );
  }

  Size? get _canvasSize {
    final ctx = _canvasKey.currentContext;
    if (ctx == null) return null;
    return ctx.size;
  }

  void _onPanStart(DragStartDetails details) {
    _currentPoints = [details.localPosition];
  }

  void _onPanUpdate(DragUpdateDetails details) {
    setState(() {
      _currentPoints = [..._currentPoints, details.localPosition];
    });
  }

  void _onPanEnd(DragEndDetails details) {
    if (_currentPoints.length < 2) {
      _currentPoints = [];
      return;
    }

    final stroke = Stroke(
      senderId: widget.nodeId ?? '??',
      color: _myColor,
      points: List.from(_currentPoints),
    );

    setState(() {
      _strokes.add(stroke);
      _currentPoints = [];
    });

    // Send over gossip
    final size = _canvasSize;
    if (size != null) {
      final encoded = stroke.encode(size);
      _log('sending stroke (${stroke.points.length} points)');
      sendSketch(stroke: encoded).catchError((e) {
        _log('send sketch error: $e');
      });
    }
  }

  void _clear() {
    setState(() => _strokes.clear());
  }

  void _cycleColor() {
    setState(() {
      final idx = _colors.indexOf(_myColor);
      _myColor = _colors[(idx + 1) % _colors.length];
    });
  }

  @override
  void dispose() {
    _subscription?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        // Toolbar
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
          child: Row(
            children: [
              IconButton(
                onPressed: _cycleColor,
                icon: Icon(Icons.palette, color: _myColor),
                tooltip: 'Change color',
              ),
              const Spacer(),
              Text(
                '${_strokes.length} strokes',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const Spacer(),
              IconButton(
                onPressed: _clear,
                icon: const Icon(Icons.delete_outline),
                tooltip: 'Clear canvas',
              ),
            ],
          ),
        ),
        // Canvas
        Expanded(
          child: GestureDetector(
            onPanStart: _onPanStart,
            onPanUpdate: _onPanUpdate,
            onPanEnd: _onPanEnd,
            child: ClipRect(
              child: CustomPaint(
                key: _canvasKey,
                painter: _SketchPainter(
                  strokes: _strokes,
                  currentPoints: _currentPoints,
                  currentColor: _myColor,
                ),
                size: Size.infinite,
              ),
            ),
          ),
        ),
      ],
    );
  }
}

class _SketchPainter extends CustomPainter {
  final List<Stroke> strokes;
  final List<Offset> currentPoints;
  final Color currentColor;

  _SketchPainter({
    required this.strokes,
    required this.currentPoints,
    required this.currentColor,
  });

  @override
  void paint(Canvas canvas, Size size) {
    // Draw completed strokes
    for (final stroke in strokes) {
      _drawStroke(canvas, stroke.points, stroke.color);
    }

    // Draw current (in-progress) stroke
    if (currentPoints.length >= 2) {
      _drawStroke(canvas, currentPoints, currentColor);
    }
  }

  void _drawStroke(Canvas canvas, List<Offset> points, Color color) {
    if (points.length < 2) return;
    final paint = Paint()
      ..color = color
      ..strokeWidth = 3.0
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round
      ..style = PaintingStyle.stroke;

    final path = Path()..moveTo(points[0].dx, points[0].dy);
    for (var i = 1; i < points.length; i++) {
      path.lineTo(points[i].dx, points[i].dy);
    }
    canvas.drawPath(path, paint);
  }

  @override
  bool shouldRepaint(covariant _SketchPainter oldDelegate) => true;
}
