import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:web_socket_channel/web_socket_channel.dart';
import 'package:http/http.dart' as http;
import 'dart:convert';
import 'dart:async';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:record/record.dart';
import 'package:just_audio/just_audio.dart';
import 'package:just_audio_media_kit/just_audio_media_kit.dart';
import 'package:media_kit/media_kit.dart';
import 'dart:io';

const kBg          = Color(0xFF0F2A58);
const kSidebarDark = Color(0xFF162E58);
const kBarTop      = Color(0xFF4A7AB8);
const kBarBot      = Color(0xFF0F3070);
const kInputTop    = Color(0xFF2A4A7A);
const kInputBot    = Color(0xFF1A3A6A);
const kAccent      = Color(0xFF4A7AB8);
const kTextPrimary = Color(0xFFDDEEFF);
const kTextMuted   = Color(0xFF5A7898);
const kOnline      = Color(0xFF5DC200);
const kBorder      = Color(0xFF0A2050);
const kMsgBg       = Colors.white;
const kMsgBorder   = Color(0xFF1A4A8A);

const List<Color> kAvatarColors = [
  Color(0xFF4A8AD4), Color(0xFF8040B0), Color(0xFF408040),
  Color(0xFFB06030), Color(0xFFB03050), Color(0xFF208080),
];
Color avatarColor(String name) {
  int h = 0;
  for (final c in name.codeUnits) h = (h * 31 + c) & 0xffff;
  return kAvatarColors[h % kAvatarColors.length];
}

String kServerHost = 'tina.humboldt-polaris.ts.net';
String get kApiBase => 'https://$kServerHost/ele-api';
String get kWsBase  => 'wss://$kServerHost/ele-ws';

String detectTransport(String host) {
  if (host.contains('.ts.net') || host.startsWith('100.')) return 'Tailscale';
  if (host == 'localhost' || host == '127.0.0.1') return 'Local';
  if (host.endsWith('.local')) return 'LAN';
  final parts = host.split('.');
  if (parts.length >= 2 && RegExp(r'^\d+$').hasMatch(parts[0])) {
    final first = int.tryParse(parts[0]) ?? 0;
    if (first == 10 || first == 172 || first == 192) return 'LAN';
  }
  return 'Internet';
}
Color transportColor(String t) {
  if (t == 'Tailscale' || t == 'Internet') return kOnline;
  if (t == 'LAN' || t == 'Local') return const Color(0xFFF0C040);
  return const Color(0xFFCC3030);
}

String formatTime(String ts) {
  try {
    final dt = DateTime.parse(ts.endsWith('Z') ? ts : '${ts}Z').toLocal();
    final h = dt.hour % 12 == 0 ? 12 : dt.hour % 12;
    final m = dt.minute.toString().padLeft(2, '0');
    final ampm = dt.hour < 12 ? 'AM' : 'PM';
    return '$h:$m $ampm';
  } catch (_) { return ''; }
}
String formatConvoTime(String ts) {
  try {
    final dt = DateTime.parse(ts.endsWith('Z') ? ts : '${ts}Z').toLocal();
    final now = DateTime.now();
    if (dt.year == now.year && dt.month == now.month && dt.day == now.day) return formatTime(ts);
    return '${dt.month}/${dt.day}';
  } catch (_) { return ''; }
}

void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();
  JustAudioMediaKit.ensureInitialized();
  final prefs = await SharedPreferences.getInstance();
  kServerHost = prefs.getString('server_host') ?? 'tina.humboldt-polaris.ts.net';
  runApp(const ELEApp());
}

class ELEApp extends StatefulWidget {
  const ELEApp({super.key});
  @override
  State<ELEApp> createState() => _ELEAppState();
}

class _ELEAppState extends State<ELEApp> {
  Widget _home = const Scaffold(body: Center(child: CircularProgressIndicator()));

  @override
  void initState() {
    super.initState();
    _tryAutologin();
  }

  Future<void> _tryAutologin() async {
    final prefs = await SharedPreferences.getInstance();
    final token = prefs.getString('token');
    final username = prefs.getString('username');
    if (token != null && username != null) {
      try {
        final res = await http.get(
          Uri.parse('$kApiBase/api/accounts?token=$token'),
        ).timeout(const Duration(seconds: 6));
        if (res.statusCode == 200) {
          setState(() => _home = ChatShell(username: username, token: token));
          return;
        }
    } catch (_) {}
    }
    setState(() => _home = const LoginScreen());
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'ELE Messenger', debugShowCheckedModeBanner: false,
    theme: ThemeData(fontFamily: 'Segoe UI',
      colorScheme: const ColorScheme.dark(surface: kBg),
      scaffoldBackgroundColor: kBg),
    home: _home);
}

class LoginScreen extends StatefulWidget {
  const LoginScreen({super.key});
  @override State<LoginScreen> createState() => _LoginScreenState();
}
class _LoginScreenState extends State<LoginScreen> {
  final _userCtrl = TextEditingController();
  final _pinCtrl  = TextEditingController();
  bool _isRegister = false, _loading = false;
  String _error = '';

  Future<void> _submit() async {
    final username = _userCtrl.text.trim(), pin = _pinCtrl.text.trim();
    if (username.isEmpty || pin.isEmpty) return;
    if (_isRegister && (pin.length != 4 || int.tryParse(pin) == null)) {
      setState(() => _error = 'PIN must be 4 digits.'); return;
    }
    setState(() { _loading = true; _error = ''; });
    try {
      final res = await http.post(
        Uri.parse('$kApiBase/api/${_isRegister ? "register" : "login"}'),
        headers: {'Content-Type': 'application/json'},
        body: jsonEncode({'username': username, 'pin': pin}));
      if (res.statusCode == 409) { setState(() { _error = 'Name already taken.'; _loading = false; }); return; }
      if (res.statusCode == 401) { setState(() { _error = 'Invalid name or PIN.'; _loading = false; }); return; }
      if (res.statusCode != 200) { setState(() { _error = 'Server error.'; _loading = false; }); return; }
      final data = jsonDecode(res.body);
    if (!mounted) return;
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString('token', data['token'] as String);
    await prefs.setString('username', data['username'] as String);
    Navigator.pushReplacement(context, MaterialPageRoute(
      builder: (_) => ChatShell(username: data['username'], token: data['token'])));
    } catch (_) { setState(() { _error = 'Server unreachable.'; _loading = false; }); }
  }

  Future<void> _editServer() async {
    final ctrl = TextEditingController(text: kServerHost);
    await showDialog(context: context, builder: (_) => AlertDialog(
      title: const Text('Message server'),
      content: TextField(
        controller: ctrl,
        decoration: const InputDecoration(labelText: 'Enter message server'),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        TextButton(onPressed: () async {
          final host = ctrl.text.trim();
          if (host.isNotEmpty) {
            final prefs = await SharedPreferences.getInstance();
            await prefs.setString('server_host', host);
            setState(() { kServerHost = host; });
          }
          if (mounted) Navigator.pop(context);
        }, child: const Text('Save')),
      ],
    ));
  }

  @override
  Widget build(BuildContext context) => Scaffold(body: Stack(children: [Container(
    decoration: const BoxDecoration(gradient: LinearGradient(
      begin: Alignment.topLeft, end: Alignment.bottomRight,
      colors: [kBg, Color(0xFF1E3A6E)])),
    child: Center(child: Container(width: 280, padding: const EdgeInsets.all(24),
      decoration: BoxDecoration(color: const Color(0xF5F0F6FF),
        border: Border.all(color: kAccent), borderRadius: BorderRadius.circular(6)),
      child: Column(mainAxisSize: MainAxisSize.min, children: [
        const Text('ELE Messenger', style: TextStyle(fontSize: 22, fontWeight: FontWeight.bold, color: Color(0xFF0F3070))),
        const SizedBox(height: 16),
        _field(_userCtrl, 'Username', false), const SizedBox(height: 10),
        _field(_pinCtrl, 'PIN', true), const SizedBox(height: 6),
        if (_error.isNotEmpty) Text(_error, style: const TextStyle(color: Color(0xFFCC0000), fontSize: 13)),
        const SizedBox(height: 10),
        SizedBox(width: double.infinity, child: ElevatedButton(
          onPressed: _loading ? null : _submit,
          style: ElevatedButton.styleFrom(backgroundColor: kAccent, foregroundColor: kTextPrimary,
            padding: const EdgeInsets.symmetric(vertical: 12),
            shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(3))),
          child: _loading
            ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2, color: Colors.white))
            : Text(_isRegister ? 'Register' : 'Sign In', style: const TextStyle(fontSize: 16)))),
        const SizedBox(height: 10),
        GestureDetector(
          onTap: () => setState(() { _isRegister = !_isRegister; _error = ''; }),
          child: Text(_isRegister ? 'Already have an account? Sign in' : 'New user? Register',
            style: const TextStyle(fontSize: 13, color: Color(0xFF1A4880), decoration: TextDecoration.underline))),
      ])))),
    Positioned(top: 12, right: 12, child: PopupMenuButton<String>(
      icon: const Icon(Icons.settings, color: Colors.white54),
      onSelected: (val) async { if (val == 'server') await _editServer(); },
      itemBuilder: (_) => [const PopupMenuItem(value: 'server', child: Text('Message server'))],
    )),
  ]));

  Widget _field(TextEditingController ctrl, String label, bool obscure) => TextField(
    controller: ctrl, obscureText: obscure,
    keyboardType: obscure ? TextInputType.number : TextInputType.text,
    inputFormatters: obscure ? [FilteringTextInputFormatter.digitsOnly, LengthLimitingTextInputFormatter(4)] : [],
    onSubmitted: (_) => _submit(),
    decoration: InputDecoration(labelText: label, labelStyle: const TextStyle(color: Color(0xFF4A7AB8)),
      border: const OutlineInputBorder(),
      focusedBorder: OutlineInputBorder(borderSide: BorderSide(color: kAccent, width: 2)),
      filled: true, fillColor: Colors.white.withOpacity(0.95),
      contentPadding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8)),
    style: const TextStyle(fontSize: 16, color: Colors.black87));
}

class ChatShell extends StatefulWidget {
  final String username, token;
  const ChatShell({super.key, required this.username, required this.token});
  @override State<ChatShell> createState() => _ChatShellState();
}
class _ChatShellState extends State<ChatShell> {
  WebSocketChannel? _ws;
  final Map<String, List<Map<String,dynamic>>> _threads = {};
  final Map<String, int> _unread = {};
  List<String> _contacts = [];
  Set<String> _online = {};
  String? _active;        // active conversation_id
  String? _activePeer;    // peer username for active convo
  bool _wsConnected = false, _showScrollBtn = false;
  String _transport = '';
  int _onlineCount = 0;
  Timer? _onlineTimer, _pingTimer;
  final _msgCtrl = TextEditingController();
  // Voice recording
  final AudioRecorder _recorder = AudioRecorder();
  bool _recActive = false;
  bool _recReview = false;
  String? _recPath;
  Duration _recDuration = Duration.zero;
  Timer? _recTimer;
  final _scroll  = ScrollController();
  final Map<String, bool> _secretChats = {};
  final Map<String, String> _groupNames = {};
  final Map<String, List<String>> _groupMembers = {};

  @override
  void initState() {
    super.initState();
    _transport = detectTransport(kServerHost);
    _loadContacts();
    _loadAllHistory();
    _connectWs();
    _scroll.addListener(() {
      final atBottom = _scroll.hasClients && _scroll.position.maxScrollExtent - _scroll.offset < 80;
      if (!atBottom != _showScrollBtn) setState(() => _showScrollBtn = !atBottom);
    });
    _onlineTimer = Timer.periodic(const Duration(seconds: 30), (_) => _loadOnline());
  }

  Future<void> _loadContacts() async {
    try {
      final res = await http.get(Uri.parse('$kApiBase/api/accounts?token=${widget.token}'));
      if (res.statusCode == 200) {
        final all = List<String>.from(jsonDecode(res.body)['accounts']);
        setState(() => _contacts = all.where((u) => u != widget.username).toList());
      }
    } catch (_) {}
    _loadOnline();
  }

  Future<void> _loadOnline() async {
    try {
      final res = await http.get(Uri.parse('$kApiBase/online'));
      if (res.statusCode == 200) {
        final online = Set<String>.from(jsonDecode(res.body)['online']);
        setState(() { _online = online; _onlineCount = online.where((u) => u != widget.username).length; });
      }
    } catch (_) {}
  }

  Future<void> _loadAllHistory() async {
    try {
      final res = await http.get(Uri.parse('$kApiBase/api/history?token=${widget.token}'));
      if (res.statusCode == 200) {
        final all = (jsonDecode(res.body)['history'] as List).cast<Map<String, dynamic>>();
        final Map<String, List<Map<String, dynamic>>> grouped = {};
        for (final m in all) {
          final cid = m['conversation_id'] as String? ??
            ((){final p = [m['from'] as String, m['to'] as String]..sort(); return p.join(':');})();
          grouped.putIfAbsent(cid, () => []).add({
            'from': m['from'] as String, 'to': m['to'] as String,
            'message': m['message'] as String, 'ts': m['timestamp'] as String,
            'image_id': m['image_id'],
            'conversation_id': cid,
          });
        }
        setState(() => _threads.addAll(grouped));
      }
    } catch (_) {}
    // Load group conversation metadata
    try {
      final res2 = await http.get(Uri.parse('$kApiBase/api/conversations?token=${widget.token}'));
      if (res2.statusCode == 200) {
        final convos = (jsonDecode(res2.body)['conversations'] as List).cast<Map<String, dynamic>>();
        for (final c in convos) {
          final cid = c['conversation_id'] as String;
          if (_cidIsGroup(cid)) {
            _groupNames[cid] = c['name'] as String? ?? cid;
            _groupMembers[cid] = List<String>.from(c['members'] as List? ?? []);
            _threads.putIfAbsent(cid, () => []);
          }
        }
        if (mounted) setState(() {});
      }
    } catch (_) {}
  }

  Future<void> _loadHistory(String contact) async {
    try {
      final res = await http.get(Uri.parse('$kApiBase/api/history?token=${widget.token}&user=$contact'));
      if (res.statusCode == 200) {
      final msgs = (jsonDecode(res.body)['history'] as List).map((m) => {
        'from': m['from'] as String, 'to': m['to'] as String, 'message': m['message'] as String, 'ts': m['timestamp'] as String,
        }).toList();
        setState(() => _threads[contact] = msgs);
      }
    } catch (_) {}
  }

  void _connectWs() {
    try {
      _ws = WebSocketChannel.connect(Uri.parse('$kWsBase/${widget.username}?token=${widget.token}'));
      setState(() => _wsConnected = true);
      _ws!.stream.listen((data) {
        final msg = jsonDecode(data) as Map<String, dynamic>;
        if (msg['type'] == 'pong') return;
        if (msg['type'] == 'secret_chat_started') {
          final with_ = msg['with'] as String?;
          if (with_ != null) setState(() => _secretChats[with_] = true);
          return;
        }
        if (msg['type'] == 'conversation_created') {
          final cid = msg['conversation_id'] as String?;
          if (cid != null && _cidIsGroup(cid)) {
            setState(() {
              _groupNames[cid] = msg['name'] as String? ?? cid;
              _groupMembers[cid] = List<String>.from(msg['members'] as List? ?? []);
              _threads.putIfAbsent(cid, () => []);
            });
          }
          return;
        }
        if (msg['type'] == 'member_added') {
          final cid = msg['conversation_id'] as String?;
          if (cid != null) {
            setState(() => _groupMembers[cid] = List<String>.from(msg['members'] as List? ?? []));
          }
          return;
        }
        final from = msg['from'] as String?;
        if (from == null || from == 'server') return;
        final incomingCid = msg['conversation_id'] as String?;
        final effectiveCid = incomingCid ?? from;
        final entry = {'from': from, 'message': msg['message'] ?? '', 'image_id': msg['image_id'], 'ts': DateTime.now().toUtc().toIso8601String(), 'conversation_id': effectiveCid};
        setState(() {
          _threads.putIfAbsent(effectiveCid, () => []).add(entry);
          if (_active != effectiveCid) _unread[effectiveCid] = (_unread[effectiveCid] ?? 0) + 1;
        });
        if (_active == effectiveCid) _scrollBottom();
      },
      onDone: () { setState(() => _wsConnected = false); Future.delayed(const Duration(seconds: 3), _connectWs); },
      onError: (_) { setState(() => _wsConnected = false); Future.delayed(const Duration(seconds: 3), _connectWs); });
      _pingTimer?.cancel();
      _pingTimer = Timer.periodic(const Duration(seconds: 30), (_) {
        try { _ws?.sink.add(jsonEncode({'type': 'ping'})); } catch (_) {}
      });
    } catch (_) {
      setState(() => _wsConnected = false);
      Future.delayed(const Duration(seconds: 5), _connectWs);
    }
  }

  void _send() {
    final msg = _msgCtrl.text.trim();
    if (msg.isEmpty || _active == null || _ws == null) return;
    final payload = _cidIsGroup(_active!)
        ? {'conversation_id': _active, 'message': msg}
        : {'to': _active, 'message': msg};
    _ws!.sink.add(jsonEncode(payload));
    final entry = {'from': widget.username, 'message': msg, 'ts': DateTime.now().toUtc().toIso8601String()};
    setState(() => _threads.putIfAbsent(_active!, () => []).add(entry));
    _msgCtrl.clear();
    _scrollBottom();
  }

  void _sendAudio(String imageId) {
    if (_active == null || _ws == null) return;
    final payload = _cidIsGroup(_active!)
        ? {'conversation_id': _active, 'message': '', 'image_id': imageId}
        : {'to': _active, 'message': '', 'image_id': imageId};
    _ws!.sink.add(jsonEncode(payload));
    final entry = {'from': widget.username, 'to': _active, 'message': '', 'image_id': imageId, 'ts': DateTime.now().toUtc().toIso8601String()};
    setState(() => _threads.putIfAbsent(_active!, () => []).add(entry));
    _scrollBottom();
  }

  void _scrollBottom() {
    Future.delayed(const Duration(milliseconds: 80), () {
      if (_scroll.hasClients) _scroll.animateTo(_scroll.position.maxScrollExtent,
        duration: const Duration(milliseconds: 200), curve: Curves.easeOut);
    });
  }

  void _selectCid(String cid, String peer) {
    setState(() { _active = cid; _activePeer = peer; _unread.remove(cid); });
    _scrollBottom();
  }

  Future<void> _selectContact(String name) async {
    final parts = [widget.username, name]..sort();
    final cid = parts.join(':');
    setState(() { _active = cid; _activePeer = name; _unread.remove(cid); _threads.putIfAbsent(cid, () => []); });
    await _loadHistory(name);
    _scrollBottom();
  }

  Future<void> _clearChat() async {
    if (_activePeer == null || _active == null) return;
    try {
      final secretParam = _cidIsSecret(_active!) ? '&secret=true' : '';
      await http.delete(Uri.parse('$kApiBase/api/history/${_activePeer!}?token=${widget.token}${secretParam}'));
      setState(() {
        _threads.remove(_active);
        _active = null;
        _activePeer = null;
      });
    } catch (_) {}
  }

  void _startSecretChat() {
    if (_activePeer == null) return;
    final parts = [widget.username, _activePeer!]..sort();
    final secretCid = 'secret:' + parts.join(':');
    setState(() {
      _threads.putIfAbsent(secretCid, () => []);
      _active = secretCid;
    });
  }

  List<String> get _activeContacts {
    final result = <String>[];
    for (final cid in _threads.keys) {
      if (_cidIsGroup(cid)) {
        result.add(cid);
        continue;
      }
      final peer = _peerFromCid(cid);
      if (peer == null) continue;
      if (_threads[cid]!.isEmpty && cid != _active) continue;
      result.add(cid);
    }
    return result;
  }
  String? _peerFromCid(String cid) {
    final parts = cid.startsWith('secret:') ? cid.substring(7).split(':') : cid.split(':');
    if (parts.length < 2) return null;
    return parts.firstWhere((p) => p != widget.username, orElse: () => parts[0]);
  }
  bool _cidIsSecret(String cid) => cid.startsWith('secret:');
  bool _cidIsGroup(String cid) => !cid.contains(':');

  @override
  void dispose() {
    _ws?.sink.close(); _msgCtrl.dispose(); _scroll.dispose();
    _onlineTimer?.cancel(); _pingTimer?.cancel();
    _recTimer?.cancel();
    _recorder.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(body: Column(children: [
    _buildTitlebar(),
    Expanded(child: Row(children: [_buildSidebar(), _buildChat()])),
    _buildStatusBar(),
  ]));

  // ── Voice recording ─────────────────────────────────────────────────────

  Future<void> _recStart() async {
    final hasPermission = await _recorder.hasPermission();
    if (!hasPermission) return;
    final path =
        '${Directory.systemTemp.path}/ele_rec_${DateTime.now().millisecondsSinceEpoch}.m4a';
    await _recorder.start(const RecordConfig(encoder: AudioEncoder.aacLc), path: path);
    setState(() {
      _recActive = true; _recReview = false;
      _recPath = null; _recDuration = Duration.zero;
    });
    _recTimer = Timer.periodic(const Duration(seconds: 1), (_) {
      setState(() => _recDuration += const Duration(seconds: 1));
      if (_recDuration.inSeconds >= 60) _recStop();
    });
  }

  Future<void> _recStop() async {
    _recTimer?.cancel();
    final path = await _recorder.stop();
    setState(() { _recActive = false; _recReview = true; _recPath = path; });
  }

  void _recDiscard() {
    if (_recPath != null) { try { File(_recPath!).deleteSync(); } catch (_) {} }
    setState(() {
      _recActive = false; _recReview = false;
      _recPath = null; _recDuration = Duration.zero;
    });
  }

  Future<void> _recSend() async {
    if (_recPath == null || _active == null) return;
    final bytes = await File(_recPath!).readAsBytes();
    final b64 = base64Encode(bytes);
    final dataUri = 'data:audio/mp4;base64,$b64';
    _recDiscard();
    try {
      final res = await http.post(
        Uri.parse('$kApiBase/api/upload?token=${widget.token}'),
        headers: {'Content-Type': 'application/json'},
        body: jsonEncode({'data': dataUri, 'mime': 'audio/mp4'}));
      if (res.statusCode != 200) return;
      final id = (jsonDecode(res.body) as Map)['id'] as String;
      _sendAudio('audio:$id');
    } catch (_) {}
  }

  String _recTimeStr() {
    final m = _recDuration.inMinutes;
    final s = (_recDuration.inSeconds % 60).toString().padLeft(2, '0');
    return '$m:$s';
  }

  Widget _buildRecordingRow() => Row(children: [
    const Icon(Icons.mic, color: Color(0xFFCC2020), size: 18),
    const SizedBox(width: 8),
    Text(_recTimeStr(),
        style: const TextStyle(color: Color(0xFFAACCEE), fontSize: 15)),
    const SizedBox(width: 8),
    Expanded(child: Container(height: 2, color: const Color(0xFF4A7AB8))),
    const SizedBox(width: 8),
    ElevatedButton(
      onPressed: _recStop,
      style: ElevatedButton.styleFrom(
        backgroundColor: const Color(0xFF993333), foregroundColor: Colors.white,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(3))),
      child: const Text('Stop', style: TextStyle(fontSize: 15))),
  ]);

  Widget _buildReviewRow() => Row(children: [
    const Icon(Icons.mic, color: Color(0xFF4A7AB8), size: 18),
    const SizedBox(width: 8),
    Text(_recTimeStr(),
        style: const TextStyle(color: Color(0xFFAACCEE), fontSize: 15)),
    const SizedBox(width: 4),
    const Text('· Ready to send',
        style: TextStyle(color: Color(0xFF7A9ABF), fontSize: 13)),
    const Spacer(),
    InkWell(
      onTap: _recDiscard,
      borderRadius: BorderRadius.circular(3),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        decoration: BoxDecoration(
          color: Colors.white.withOpacity(0.08),
          border: Border.all(color: Colors.white.withOpacity(0.2)),
          borderRadius: BorderRadius.circular(3)),
        child: const Icon(Icons.close, color: Color(0xFFFF6666), size: 18))),
    const SizedBox(width: 6),
    ElevatedButton(
      onPressed: _recSend,
      style: ElevatedButton.styleFrom(
        backgroundColor: kAccent, foregroundColor: kTextPrimary,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(3))),
      child: const Text('Send', style: TextStyle(fontSize: 15))),
  ]);

  Widget _buildMessageContent(Map<String, dynamic> m) {
    final imageId = m['image_id'] as String?;
    final message = m['message'] as String? ?? '';
    if (imageId != null && imageId.startsWith('audio:')) {
      final id = imageId.substring(6);
      return _AudioPlayerWidget(
          url: '$kApiBase/api/image/$id?token=${widget.token}');
    }
    if (imageId != null && imageId.isNotEmpty) {
      return Image.network(
        '$kApiBase/api/image/$imageId?token=${widget.token}',
        height: 180, fit: BoxFit.contain,
        errorBuilder: (_, __, ___) =>
            const Text('[image]', style: TextStyle(color: Colors.grey)));
    }
    return Text(message,
        style: const TextStyle(
            fontSize: 16, color: Color(0xFF222222), height: 1.5));
  }

  Widget _buildTitlebar() => Container(
    height: 40,
    decoration: const BoxDecoration(
      gradient: LinearGradient(begin: Alignment.topCenter, end: Alignment.bottomCenter,
        colors: [kBarTop, Color(0xFF2A5A9A), Color(0xFF1A4A8A), kBarBot]),
      border: Border(bottom: BorderSide(color: Color(0xFF0A2060)))),
    padding: const EdgeInsets.symmetric(horizontal: 10),
    child: Row(children: [
      Image.asset('assets/icon.png', width: 18, height: 18),
      const SizedBox(width: 8),
      Expanded(child: Text('ELE Messenger \u2014 ${widget.username}',
        style: const TextStyle(color: kTextPrimary, fontSize: 15, fontWeight: FontWeight.bold,
          shadows: [Shadow(color: Colors.black45, offset: Offset(0,1), blurRadius: 3)]))),
      PopupMenuButton<String>(
        icon: const Icon(Icons.menu, color: kTextPrimary),
        color: const Color(0xFF1E3A6E),
        onSelected: (val) async {
          if (val == 'signout') {
            _ws?.sink.close();
            final prefs = await SharedPreferences.getInstance();
            await prefs.clear();
            Navigator.pushReplacement(context, MaterialPageRoute(builder: (_) => const LoginScreen()));
          }
          else if (val == 'secret') _startSecretChat();
          else if (val == 'settings') showDialog(context: context, builder: (_) => AlertDialog(
            backgroundColor: const Color(0xFF1E3A6E),
            title: const Text('Settings', style: TextStyle(color: kTextPrimary, fontSize: 18, fontWeight: FontWeight.bold)),
            content: const Text('Coming soon.', style: TextStyle(color: kTextPrimary)),
            actions: [TextButton(onPressed: () => Navigator.pop(context), child: const Text('OK', style: TextStyle(color: kAccent)))]));
          else if (val == 'clearchat' && _active != null) await _clearChat();
          else if (val == 'about') _showAbout();
        },
        itemBuilder: (_) => [
          if (_activePeer != null && !_cidIsSecret(_active ?? ''))
            const PopupMenuItem(value: 'secret', child: Text(
              'Start secret chat',
              style: TextStyle(color: kTextPrimary, fontSize: 14))),
          const PopupMenuItem(value: 'clearchat', child: Text('Delete chat for both', style: TextStyle(color: Color(0xFFFF6666), fontSize: 14))),
          const PopupMenuItem(value: 'settings', child: Text('Settings', style: TextStyle(color: kTextPrimary, fontSize: 14))),
          const PopupMenuItem(value: 'about', child: Text('About', style: TextStyle(color: kTextPrimary, fontSize: 14))),
          const PopupMenuDivider(),
          const PopupMenuItem(value: 'signout', child: Text('Sign out', style: TextStyle(color: kTextPrimary, fontSize: 14))),
        ]),
    ]));

  void _showAbout() => showDialog(context: context, builder: (_) => AlertDialog(
    backgroundColor: const Color(0xFF1E3A6E),
    title: const Text('ELE Messenger', style: TextStyle(color: kTextPrimary, fontSize: 18, fontWeight: FontWeight.bold)),
    content: Column(mainAxisSize: MainAxisSize.min, children: [
      Image.asset('assets/coverart.png', width: 160, height: 160),
      const SizedBox(height: 8),
      Text('v1.3.2', style: TextStyle(color: Color(0xFF7A9ABF), fontSize: 13)),
      SizedBox(height: 4),
      Text(kServerHost, style: TextStyle(color: Color(0xFF7A9ABF), fontSize: 13)),
      SizedBox(height: 4),
      Text('GPL 3.0', style: TextStyle(color: Color(0xFF4A6888), fontSize: 12)),
    ]),
    actions: [TextButton(onPressed: () => Navigator.pop(context),
      child: const Text('Close', style: TextStyle(color: kAccent)))],
  ));

  Widget _buildSidebar() {
    final contacts = _activeContacts;
    final tColor = transportColor(_transport);
    return Container(
      width: 260,
      decoration: const BoxDecoration(
        gradient: LinearGradient(begin: Alignment.topCenter, end: Alignment.bottomCenter,
          colors: [Color(0xFF1E3A6E), kSidebarDark]),
        border: Border(right: BorderSide(color: kBorder))),
      child: Column(children: [
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
          decoration: const BoxDecoration(
            gradient: LinearGradient(colors: [Color(0xFF2A4A7A), Color(0xFF1A3A6A)]),
            border: Border(bottom: BorderSide(color: kBorder))),
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            const Text('Conversations', style: TextStyle(fontSize: 15, fontWeight: FontWeight.bold, color: Color(0xFFAACCEE))),
            const SizedBox(height: 2),
            Row(children: [
              Container(width: 7, height: 7, margin: const EdgeInsets.only(right: 4),
                decoration: BoxDecoration(color: tColor, shape: BoxShape.circle)),
              Text(_transport, style: const TextStyle(fontSize: 11, color: Color(0xFFAACCEE), letterSpacing: 0.3)),
            ]),
          ])),
        GestureDetector(onTap: _showNewChatModal,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 11),
            decoration: const BoxDecoration(color: Color(0xFF1A4A8A),
              border: Border(bottom: BorderSide(color: Color(0xFF0A2050)))),
            child: const Row(children: [
              Icon(Icons.add, color: kTextPrimary, size: 16), SizedBox(width: 6),
              Text('New Chat', style: TextStyle(fontSize: 14, color: kTextPrimary))]))),
        Expanded(child: ListView.builder(
          itemCount: contacts.length,
          itemBuilder: (_, i) => _buildConvoRow(contacts[i]))),
      ]));
  }

  void _showNewChatModal() {
    final activePeers = _activeContacts.map((cid) => _peerFromCid(cid)).toSet();
    final available = _contacts.where((c) => !activePeers.contains(c)).toList();
    final selected = <String>{};
    bool isSecret = false;
    showDialog(
      context: context,
      builder: (_) => StatefulBuilder(
        builder: (ctx, setS) => AlertDialog(
          backgroundColor: const Color(0xFF1E3A6E),
          title: const Text('New Chat', style: TextStyle(color: Color(0xFFAACCEE), fontSize: 15)),
          content: SizedBox(width: 260, child: available.isEmpty
            ? const Text('No other users found.', style: TextStyle(color: Color(0xFF5A7898)))
            : Column(mainAxisSize: MainAxisSize.min, children: [
                ConstrainedBox(
                  constraints: const BoxConstraints(maxHeight: 240),
                  child: ListView.builder(
                    shrinkWrap: true,
                    itemCount: available.length,
                    itemBuilder: (_, i) {
                      final name = available[i];
                      final disabled = isSecret && selected.isNotEmpty && !selected.contains(name);
                      return Padding(
                        padding: const EdgeInsets.symmetric(vertical: 4),
                        child: CheckboxListTile(
                          value: selected.contains(name),
                          onChanged: disabled ? null : (v) => setS(() {
                            if (v == true) { selected.add(name); } else selected.remove(name);
                          }),
                          title: Text(name, style: TextStyle(
                            color: disabled ? const Color(0xFF3A5878) : const Color(0xFFAACCEE),
                            fontSize: 14)),
                          secondary: CircleAvatar(radius: 20, backgroundColor: avatarColor(name),
                            child: Text(name[0].toUpperCase(),
                              style: const TextStyle(color: Colors.white, fontWeight: FontWeight.bold, fontSize: 15))),
                          activeColor: kAccent,
                          checkColor: Colors.white,
                          controlAffinity: ListTileControlAffinity.trailing,
                        ));
                    })),
                const Divider(color: Color(0xFF5A7AB8), thickness: 1, height: 24),
                CheckboxListTile(
                  value: isSecret,
                  onChanged: selected.length == 1 ? (v) => setS(() => isSecret = v ?? false) : null,
                  title: Text('Secret chat',
                    style: TextStyle(
                      color: selected.length != 1 ? const Color(0xFF3A5878) : const Color(0xFFAACC88),
                      fontSize: 13)),
                  secondary: const Icon(Icons.lock, color: Color(0xFFAACC88), size: 16),
                  activeColor: const Color(0xFFAACC88),
                  checkColor: Colors.white,
                  controlAffinity: ListTileControlAffinity.trailing,
                  dense: true,
                ),
              ])),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context),
              child: const Text('Cancel', style: TextStyle(color: kAccent))),
            TextButton(
              onPressed: selected.isEmpty ? null : () async {
                Navigator.pop(context);
                if (isSecret && selected.length == 1) {
                  final peer = selected.first;
                  final parts = [widget.username, peer]..sort();
                  final secretCid = 'secret:' + parts.join(':');
                  await _selectContact(peer);
                  setState(() {
                    _threads.putIfAbsent(secretCid, () => []);
                    _active = secretCid;
                  });
                } else if (selected.length == 1) {
                  await _selectContact(selected.first);
                } else {
                  // Group chat — call server to create conversation
                  try {
                    final res = await http.post(
                      Uri.parse('$kApiBase/api/conversation?token=${widget.token}'),
                      headers: {'Content-Type': 'application/json'},
                      body: jsonEncode({'members': selected.toList()}));
                    if (res.statusCode == 200) {
                      final data = jsonDecode(res.body);
                      final cid = data['conversation_id'] as String;
                      final name = data['name'] as String;
                      final members = List<String>.from(data['members'] as List);
                      setState(() {
                        _groupNames[cid] = name;
                        _groupMembers[cid] = members;
                        _threads.putIfAbsent(cid, () => []);
                        _active = cid;
                        _activePeer = null;
                      });
                    }
                  } catch (_) {}
                }
              },
              child: const Text('Start', style: TextStyle(color: kAccent))),
          ])));
  }

  Widget _buildConvoRow(String name) {
    final isActive = _active == name;
    final isGroup  = _cidIsGroup(name);
    final peer     = isGroup ? null : _peerFromCid(name);
    final displayName = isGroup ? (_groupNames[name] ?? name) : (peer ?? name);
    final isOnline = isGroup ? false : _online.contains(peer ?? name);
    final unread   = _unread[name] ?? 0;
    final msgs     = _threads[name] ?? [];
    final isSecret = _cidIsSecret(name);
    final preview  = msgs.isNotEmpty ? msgs.last['message'] as String : '';
    final ts       = msgs.isNotEmpty ? formatConvoTime(msgs.last['ts'] as String) : '';
    return GestureDetector(
      onTap: () {
        if (isGroup) { setState(() { _active = name; _activePeer = null; _unread.remove(name); }); _scrollBottom(); }
        else if (peer != null) _selectCid(name, peer);
      },
      child: Container(
        height: 72,
        padding: EdgeInsets.only(left: isActive ? 11 : 14, right: 14, top: 10, bottom: 10),
        decoration: BoxDecoration(
          color: isActive ? const Color(0x406080DC) : Colors.transparent,
          border: Border(
            left: BorderSide(color: isActive ? const Color(0xFF6AACF0) : Colors.transparent, width: 3),
            bottom: const BorderSide(color: Color(0x33648CCC)))),
        child: Row(children: [
          Container(width: 8, height: 8, margin: const EdgeInsets.only(right: 8, top: 2),
            decoration: BoxDecoration(
              color: isOnline ? kOnline : const Color(0xFF555555), shape: BoxShape.circle,
              border: Border.all(color: isOnline ? const Color(0xFF3A9000) : const Color(0xFF444444)))),
          isGroup
              ? CircleAvatar(radius: 20, backgroundColor: const Color(0xFF4A4A8A),
                  child: const Icon(Icons.group, color: Colors.white, size: 18))
              : CircleAvatar(radius: 20, backgroundColor: avatarColor(displayName),
                  child: Text(displayName[0].toUpperCase(),
                    style: const TextStyle(color: Colors.white, fontWeight: FontWeight.bold, fontSize: 15))),
          const SizedBox(width: 10),
          Expanded(child: Column(crossAxisAlignment: CrossAxisAlignment.start, mainAxisAlignment: MainAxisAlignment.center, children: [
            Row(children: [
              Text(displayName, style: TextStyle(fontSize: 14,
                color: unread > 0 ? kTextPrimary : const Color(0xFFAACCEE),
                fontWeight: unread > 0 ? FontWeight.bold : FontWeight.normal)),
              if (isSecret) ...[
                const SizedBox(width: 4),
                const Icon(Icons.lock, color: Color(0xFFAACC88), size: 12),
              ],
            ]),
            if (preview.isNotEmpty) Text(preview, maxLines: 1, overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: 12, color: unread > 0 ? const Color(0xFF88AACC) : kTextMuted)),
          ])),
          Column(mainAxisAlignment: MainAxisAlignment.center, crossAxisAlignment: CrossAxisAlignment.end, children: [
            if (ts.isNotEmpty) Text(ts, style: const TextStyle(fontSize: 11, color: Color(0xFF4A6888))),
            if (unread > 0) ...[const SizedBox(height: 4),
              Container(padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
                decoration: BoxDecoration(color: const Color(0xFFD04040), borderRadius: BorderRadius.circular(8)),
                child: Text('$unread', style: const TextStyle(color: Colors.white, fontSize: 11, fontWeight: FontWeight.bold)))],
          ]),
        ])));
  }

  Widget _buildChat() => Expanded(child: Column(children: [
    _buildChatHeader(),
    Expanded(child: Stack(children: [
      _buildMessages(),
      if (_showScrollBtn) Positioned(bottom: 12, right: 12,
        child: GestureDetector(
          onTap: () => _scroll.animateTo(_scroll.position.maxScrollExtent,
            duration: const Duration(milliseconds: 200), curve: Curves.easeOut),
          child: Container(width: 40, height: 40,
            decoration: BoxDecoration(color: const Color(0xFF1A4A8A), shape: BoxShape.circle,
              border: Border.all(color: kAccent, width: 2),
              boxShadow: [BoxShadow(color: Colors.black.withOpacity(0.4), blurRadius: 6)]),
            child: const Icon(Icons.arrow_downward, color: kTextPrimary, size: 20)))),
    ])),
    if (_active != null) _buildInputRow(),
  ]));

  Widget _buildChatHeader() => Container(
    height: 40, padding: const EdgeInsets.symmetric(horizontal: 12),
    decoration: const BoxDecoration(
      gradient: LinearGradient(colors: [Color(0xFF2A4A7A), Color(0xFF1A3A6A)]),
      border: Border(bottom: BorderSide(color: kBorder))),
    child: Row(children: [
      if (_active != null) ...[
        Container(width: 7, height: 7, margin: const EdgeInsets.only(right: 8),
          decoration: BoxDecoration(
            color: _online.contains(_activePeer ?? '') ? kOnline : const Color(0xFF555555), shape: BoxShape.circle)),
        Text(_active != null && _cidIsGroup(_active!) ? (_groupNames[_active!] ?? _active!) : 'Chat with ${_activePeer ?? _active!}', style: const TextStyle(fontSize: 15, fontWeight: FontWeight.bold, color: Color(0xFFAACCEE))),
        if (_cidIsSecret(_active!)) ...[
          const SizedBox(width: 6),
          const Icon(Icons.lock, color: Color(0xFFAACC88), size: 14),
        ],
      ]]));

  Widget _buildMessages() {
    if (_active == null) return Container(color: kMsgBg,
      child: const Center(child: Text('Select a conversation to start chatting',
        style: TextStyle(color: Color(0xFF9AB0CC), fontSize: 15, fontStyle: FontStyle.italic))));
    final msgs = _threads[_active!] ?? [];
    final groups = <Map<String, dynamic>>[];
    for (final m in msgs) {
      final from = m['from'] as String;
      if (groups.isEmpty || groups.last['from'] != from) {
        groups.add({'from': from, 'messages': <Map<String,dynamic>>[m]});
      } else {
        (groups.last['messages'] as List<Map<String,dynamic>>).add(m);
      }
    }
    return Container(
      decoration: const BoxDecoration(color: kMsgBg,
        border: Border(top: BorderSide(color: kMsgBorder, width: 3),
                       right: BorderSide(color: kMsgBorder, width: 3))),
      child: ListView.builder(
        controller: _scroll, padding: const EdgeInsets.all(12),
        itemCount: groups.length,
        itemBuilder: (_, i) {
          final g = groups[i];
          final from = g['from'] as String;
          final isMe = from == widget.username;
          final groupMsgs = g['messages'] as List<Map<String,dynamic>>;
          return Padding(padding: const EdgeInsets.only(bottom: 14), child: Column(
            crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(from, style: TextStyle(fontWeight: FontWeight.bold, fontSize: 16,
                color: isMe ? const Color(0xFFA03800) : const Color(0xFF0F3070))),
              const SizedBox(height: 2),
              ...groupMsgs.map((m) => Padding(padding: const EdgeInsets.only(top: 1),
                child: Row(crossAxisAlignment: CrossAxisAlignment.baseline, textBaseline: TextBaseline.alphabetic, children: [
                  const Text('\u00b7', style: TextStyle(fontSize: 8, color: Color(0xFFAAAAAA))),
                  const SizedBox(width: 6),
                  _buildMessageContent(m),
                  const SizedBox(width: 4),
                  Text(formatTime(m['ts'] as String), style: const TextStyle(fontSize: 12, color: Color(0xFFAAAAAA))),
                ]))),
            ]));
        }));
  }

  Widget _buildInputRow() => Container(
    padding: const EdgeInsets.all(8),
    decoration: const BoxDecoration(
      gradient: LinearGradient(colors: [kInputTop, kInputBot]),
      border: Border(top: BorderSide(color: kBorder))),
    child: _recReview
        ? _buildReviewRow()
        : _recActive
            ? _buildRecordingRow()
            : _buildNormalInputRow());

  Widget _buildNormalInputRow() => Row(children: [
    Expanded(child: TextField(controller: _msgCtrl, onSubmitted: (_) => _send(),
      style: const TextStyle(fontSize: 16, color: Colors.black87),
      decoration: InputDecoration(filled: true, fillColor: Colors.white.withOpacity(0.95),
        border: OutlineInputBorder(borderRadius: BorderRadius.circular(3), borderSide: BorderSide.none),
        contentPadding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8), isDense: true))),
    const SizedBox(width: 6),
    InkWell(
      onTap: _recStart,
      borderRadius: BorderRadius.circular(3),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        decoration: BoxDecoration(
          color: Colors.white.withOpacity(0.12),
          border: Border.all(color: Colors.white.withOpacity(0.25)),
          borderRadius: BorderRadius.circular(3)),
        child: const Icon(Icons.mic, color: Color(0xFF4A7AB8), size: 18))),
    const SizedBox(width: 6),
    ElevatedButton(onPressed: _send,
      style: ElevatedButton.styleFrom(backgroundColor: kAccent, foregroundColor: kTextPrimary,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(3))),
      child: const Text('Send', style: TextStyle(fontSize: 15))),
  ]);

  Widget _buildStatusBar() => Container(
    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
    decoration: const BoxDecoration(gradient: LinearGradient(colors: [Color(0xFF1A3A6A), kBg])),
    child: Row(mainAxisAlignment: MainAxisAlignment.center, children: [
      Container(width: 7, height: 7, margin: const EdgeInsets.only(right: 5),
        decoration: BoxDecoration(color: transportColor(_transport), shape: BoxShape.circle)),
      Text(_wsConnected
        ? 'Connected as ${widget.username} \u00b7 $_onlineCount contact${_onlineCount == 1 ? "" : "s"} online'
        : 'Reconnecting...',
        style: const TextStyle(fontSize: 12, color: Color(0xFF4A6888))),
    ]));
}

// ── Audio player widget ───────────────────────────────────────────────────────

class _AudioPlayerWidget extends StatefulWidget {
  final String url;
  const _AudioPlayerWidget({required this.url});
  @override
  State<_AudioPlayerWidget> createState() => _AudioPlayerWidgetState();
}

class _AudioPlayerWidgetState extends State<_AudioPlayerWidget> {
  final AudioPlayer _player = AudioPlayer();
  bool _playing = false;
  bool _loading = false;
  Duration _pos = Duration.zero;
  Duration _dur = Duration.zero;

  @override
  void initState() {
    super.initState();
    _player.positionStream.listen((p) {
      if (mounted) setState(() => _pos = p);
    });
    _player.durationStream.listen((d) {
      if (mounted) setState(() => _dur = d ?? Duration.zero);
    });
    _player.playerStateStream.listen((s) {
      if (mounted) setState(() => _playing = s.playing);
      if (s.processingState == ProcessingState.completed) {
        _player.seek(Duration.zero);
        _player.stop();
      }
    });
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      try {
        print('[audio] setUrl: ' + widget.url);
        await _player.setUrl(widget.url);
        print('[audio] setUrl complete, dur=' + _dur.toString());
      } catch (e) {
        print('[audio] setUrl error: ' + e.toString());
      }
    });
  }

  @override
  void dispose() { _player.dispose(); super.dispose(); }

  Future<void> _toggle() async {
    print('[audio] _toggle called, _playing=' + _playing.toString());
    if (_playing) {
      await _player.pause();
    } else {
      print('[audio] calling play()');
      await _player.play();
      print('[audio] play() returned');
    }
  }

  String _fmt(Duration d) {
    final m = d.inMinutes;
    final s = (d.inSeconds % 60).toString().padLeft(2, '0');
    return '$m:$s';
  }

  @override
  Widget build(BuildContext context) {
    final progress = _dur.inMilliseconds > 0
        ? _pos.inMilliseconds / _dur.inMilliseconds
        : 0.0;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
      decoration: BoxDecoration(
        color: const Color(0xFFE8EEF8),
        borderRadius: BorderRadius.circular(6),
        border: Border.all(color: const Color(0xFFAABBDD))),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        InkWell(
          onTap: _loading ? null : _toggle,
          child: Container(
            width: 32, height: 32,
            decoration: const BoxDecoration(
                color: Color(0xFF4A7AB8), shape: BoxShape.circle),
            child: _loading
                ? const Center(
                    child: SizedBox(width: 14, height: 14,
                      child: CircularProgressIndicator(
                          strokeWidth: 2, color: Colors.white)))
                : Icon(_playing ? Icons.pause : Icons.play_arrow,
                    color: Colors.white, size: 20))),
        const SizedBox(width: 8),
        Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: 120,
              child: LinearProgressIndicator(
                value: progress,
                backgroundColor: const Color(0xFFCCDDEE),
                valueColor: const AlwaysStoppedAnimation<Color>(
                    Color(0xFF4A7AB8)),
                minHeight: 3)),
            const SizedBox(height: 3),
            Text(
              _dur > Duration.zero
                  ? '${_fmt(_pos)} / ${_fmt(_dur)}'
                  : '🎤 Voice note',
              style: const TextStyle(
                  fontSize: 11, color: Color(0xFF4A6888))),
          ]),
      ]));
  }
}
