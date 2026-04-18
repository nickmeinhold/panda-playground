import 'dart:async';
import 'dart:developer' as developer;

import 'package:flutter/material.dart';
import 'package:panda_playground/sketch_screen.dart';
import 'package:panda_playground/src/rust/api/chat.dart';
import 'package:panda_playground/src/rust/frb_generated.dart';

void _log(String msg) {
  developer.log(msg, name: 'panda');
  debugPrint('[panda] $msg');
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  runApp(const PandaPlaygroundApp());
}

class PandaPlaygroundApp extends StatelessWidget {
  const PandaPlaygroundApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Panda Playground',
      theme: ThemeData(
        colorSchemeSeed: Colors.deepPurple,
        useMaterial3: true,
        brightness: Brightness.dark,
      ),
      home: const PlaygroundHome(),
    );
  }
}

/// Root widget: starts the node, then shows tabs for Chat and Sketch.
class PlaygroundHome extends StatefulWidget {
  const PlaygroundHome({super.key});

  @override
  State<PlaygroundHome> createState() => _PlaygroundHomeState();
}

class _PlaygroundHomeState extends State<PlaygroundHome> {
  String? _nodeId;
  bool _starting = true;
  String? _error;

  @override
  void initState() {
    super.initState();
    _initNode();
  }

  Future<void> _initNode() async {
    try {
      _log('calling startNode()...');
      final id = await startNode();
      _log('node started with ID: $id');
      setState(() {
        _nodeId = id;
        _starting = false;
      });
    } catch (e) {
      _log('init error: $e');
      setState(() {
        _error = e.toString();
        _starting = false;
      });
    }
  }

  @override
  void dispose() {
    stopNode();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_starting) {
      return Scaffold(
        appBar: AppBar(title: const Text('Panda Playground')),
        body: const Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              CircularProgressIndicator(),
              SizedBox(height: 16),
              Text('Starting p2panda node...'),
            ],
          ),
        ),
      );
    }

    if (_error != null) {
      return Scaffold(
        appBar: AppBar(title: const Text('Panda Playground')),
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Text(
              'Error: $_error',
              style: TextStyle(color: Theme.of(context).colorScheme.error),
            ),
          ),
        ),
      );
    }

    return DefaultTabController(
      length: 2,
      child: Scaffold(
        appBar: AppBar(
          title: Row(
            children: [
              const Text('Panda Playground'),
              const SizedBox(width: 8),
              Chip(
                label: Text(_nodeId!),
                labelStyle: const TextStyle(fontSize: 11),
                visualDensity: VisualDensity.compact,
              ),
            ],
          ),
          bottom: const TabBar(
            tabs: [
              Tab(icon: Icon(Icons.chat_bubble_outline), text: 'Chat'),
              Tab(icon: Icon(Icons.brush_outlined), text: 'Sketch'),
            ],
          ),
        ),
        body: TabBarView(
          children: [
            ChatTab(nodeId: _nodeId!),
            SketchScreen(nodeId: _nodeId),
          ],
        ),
      ),
    );
  }
}

/// Chat tab — subscribes to gossip chat messages.
class ChatTab extends StatefulWidget {
  final String nodeId;

  const ChatTab({super.key, required this.nodeId});

  @override
  State<ChatTab> createState() => _ChatTabState();
}

class _ChatTabState extends State<ChatTab> with AutomaticKeepAliveClientMixin {
  final TextEditingController _controller = TextEditingController();
  final List<ChatMessage> _messages = [];
  final ScrollController _scrollController = ScrollController();
  StreamSubscription<String>? _chatSubscription;

  @override
  bool get wantKeepAlive => true;

  @override
  void initState() {
    super.initState();
    _messages.add(ChatMessage(
      sender: 'system',
      text: 'Node started. You are ${widget.nodeId}. Waiting for pandas nearby...',
      isSystem: true,
    ));
    _subscribe();
  }

  void _subscribe() {
    _log('calling subscribeChat()...');
    _chatSubscription = subscribeChat().listen(
      (raw) {
        _log('stream event: "$raw"');
        final colonIndex = raw.indexOf(':');
        if (colonIndex == -1) return;

        final sender = raw.substring(0, colonIndex);
        final text = raw.substring(colonIndex + 1);

        if (sender == widget.nodeId) return;

        _log('displaying message from $sender: "$text"');
        setState(() {
          _messages.add(ChatMessage(sender: sender, text: text));
        });
        _scrollToBottom();
      },
      onError: (e) => _log('stream error: $e'),
      onDone: () => _log('stream closed'),
    );
    _log('subscribeChat() listener attached');
  }

  Future<void> _sendMessage() async {
    final text = _controller.text.trim();
    if (text.isEmpty) return;

    _controller.clear();

    setState(() {
      _messages.add(ChatMessage(
        sender: widget.nodeId,
        text: text,
        isMe: true,
      ));
    });
    _scrollToBottom();

    try {
      _log('calling sendMessage("$text")...');
      await sendMessage(message: text);
      _log('sendMessage completed');
    } catch (e) {
      _log('sendMessage error: $e');
      setState(() {
        _messages.add(ChatMessage(
          sender: 'system',
          text: 'Failed to send: $e',
          isSystem: true,
        ));
      });
    }
  }

  void _scrollToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scrollController.hasClients) {
        _scrollController.animateTo(
          _scrollController.position.maxScrollExtent,
          duration: const Duration(milliseconds: 200),
          curve: Curves.easeOut,
        );
      }
    });
  }

  @override
  void dispose() {
    _chatSubscription?.cancel();
    _controller.dispose();
    _scrollController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    super.build(context);
    return Column(
      children: [
        Expanded(
          child: ListView.builder(
            controller: _scrollController,
            padding: const EdgeInsets.all(16),
            itemCount: _messages.length,
            itemBuilder: (context, index) {
              final msg = _messages[index];
              return _MessageBubble(message: msg);
            },
          ),
        ),
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
          decoration: BoxDecoration(
            color: Theme.of(context).colorScheme.surfaceContainerHighest,
          ),
          child: SafeArea(
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _controller,
                    decoration: const InputDecoration(
                      hintText: 'Say something...',
                      border: InputBorder.none,
                    ),
                    onSubmitted: (_) => _sendMessage(),
                    textInputAction: TextInputAction.send,
                  ),
                ),
                IconButton(
                  onPressed: _sendMessage,
                  icon: const Icon(Icons.send),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

class ChatMessage {
  final String sender;
  final String text;
  final bool isMe;
  final bool isSystem;

  ChatMessage({
    required this.sender,
    required this.text,
    this.isMe = false,
    this.isSystem = false,
  });
}

class _MessageBubble extends StatelessWidget {
  final ChatMessage message;

  const _MessageBubble({required this.message});

  @override
  Widget build(BuildContext context) {
    if (message.isSystem) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 4),
        child: Center(
          child: Text(
            message.text,
            style: TextStyle(
              color: Theme.of(context).colorScheme.onSurfaceVariant,
              fontSize: 12,
              fontStyle: FontStyle.italic,
            ),
            textAlign: TextAlign.center,
          ),
        ),
      );
    }

    return Align(
      alignment: message.isMe ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
        constraints: BoxConstraints(
          maxWidth: MediaQuery.of(context).size.width * 0.75,
        ),
        decoration: BoxDecoration(
          color: message.isMe
              ? Theme.of(context).colorScheme.primaryContainer
              : Theme.of(context).colorScheme.secondaryContainer,
          borderRadius: BorderRadius.circular(16),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (!message.isMe)
              Padding(
                padding: const EdgeInsets.only(bottom: 4),
                child: Text(
                  message.sender,
                  style: TextStyle(
                    fontSize: 11,
                    fontWeight: FontWeight.bold,
                    color: Theme.of(context).colorScheme.primary,
                  ),
                ),
              ),
            Text(message.text),
          ],
        ),
      ),
    );
  }
}
