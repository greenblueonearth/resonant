# Resonant v2

Resonant v2 là một bản demo blockchain viết bằng Rust với cấu trúc gọn gàng và mô-đun hơn, được thiết kế để dễ hiểu, mở rộng và chạy benchmark cục bộ.

## Điểm nổi bật
- cấu trúc mô-đun: `wallet`, `ledger`, `consensus`, `p2p`, `main`
- xử lý giao dịch trong bộ nhớ nhanh hơn
- công cụ benchmark theo lô
- mô hình giao dịch lấy cảm hứng từ quyền riêng tư, sử dụng cam kết ẩn danh (stealth commitment) và nullifier
- tạo khối dựa trên hash với BLAKE3
- lựa chọn người đề xuất khối có tính xác định, dựa trên trọng số stake
- phiếu biểu quyết của validator được ký bằng Ed25519 và chứng chỉ hoàn tất có trọng số stake
- phạt hành vi ký hai thông điệp mâu thuẫn (equivocation slashing) và trì hoãn việc rút stake
- khám phá peer qua bootstrap, mã hóa Noise và xác thực peer theo allowlist Ed25519
- xác minh vote/chứng chỉ đồng thuận và chỉ áp dụng block sau khi đạt finality

## Bắt đầu nhanh

```bash
cargo run -- status
cargo run -- balance alice
cargo run -- stake alice 10000
cargo run -- wallet list
cargo run -- wallet create tester
cargo run -- transfer alice bob 250
cargo run -- transfer-signed node-a.json <RECIPIENT_ADDRESS> 250
cargo run -- mine
cargo run -- pending
cargo run -- benchmark 20000 2000
```

## Chạy nút P2P

Tạo danh tính cho mỗi nút. Tệp chứa khóa riêng, được tạo với quyền `0600`; giữ tệp an toàn và không chia sẻ khóa riêng:

```bash
cargo run -- p2p identity-create node-a.json node-a
cargo run -- p2p identity-create node-b.json node-b
```

Lệnh in ra `address` và `public_key`. Stake và đăng ký validator bằng địa chỉ/khóa công khai; đăng ký chỉ cần public key, không cần sao chép khóa riêng validator sang các nút khác:

```bash
cargo run -- stake <ADDRESS_A> 7000
cargo run -- stake <ADDRESS_B> 3000
cargo run -- p2p validator register <ADDRESS_A> <PUBLIC_KEY_A> 7000
cargo run -- p2p validator register <ADDRESS_B> <PUBLIC_KEY_B> 3000
```

Giao dịch được phát tán và xác minh trong mạng phải được ký bởi chủ tài khoản. Dùng tệp wallet để tạo giao dịch đã ký:

```bash
cargo run -- transfer-signed node-a.json <RECIPIENT_ADDRESS> 250
```

Lệnh `transfer SENDER RECIPIENT AMOUNT` vẫn dành cho demo cục bộ và tạo giao dịch không có chữ ký mật mã; node đồng thuận P2P sẽ từ chối block chứa các giao dịch đó.

Tạo/cấu hình các identity, stake và validator trong cùng một trạng thái khởi tạo; sau đó đồng bộ `.resonant-ledger.json` và `.resonant-consensus.json` đó đến từng máy. Giữ tệp khóa riêng của từng nút trên máy sở hữu nó. Chạy các nút trong những thư mục riêng, ghim public key của peer bằng `--trust`, và chỉ đề xuất block sau khi các peer đã kết nối:

```bash
cargo run -- p2p node 0.0.0.0:9000 node-a.json \
  --advertise 203.0.113.10:9000 --trust <PUBLIC_KEY_B>
cargo run -- p2p node 0.0.0.0:9001 node-b.json \
  --advertise 203.0.113.11:9001 --trust <PUBLIC_KEY_A> \
  --seed 203.0.113.10:9000
# Khi các peer đã chạy, khởi động lại một nút với --propose-on-start
```

Thay các địa chỉ ví dụ bằng địa chỉ có thể truy cập thực tế. `--advertise` là địa chỉ peer thông báo cho các nút khác; cần cấu hình khi bind vào địa chỉ wildcard hoặc khi dùng NAT/port forwarding. `--propose-on-start` tạo và phát một block ứng viên khi nút khởi động. Validator đã đăng ký sẽ tự ký vote cho block hợp lệ; nút xác minh vote/chứng chỉ và chỉ cập nhật sổ cái sau khi có chứng chỉ finality hợp lệ. CLI giữ khóa trạng thái trong suốt thời gian chạy nút. Không dùng lệnh `mine` cục bộ trên các nút mạng; lệnh đó bỏ qua finality P2P.

Lớp truyền tải dùng Noise XX (`25519/ChaChaPoly/BLAKE2s`), gắn danh tính Ed25519 vào handshake mới và chỉ chấp nhận peer có public key trong allowlist. Mỗi khung tối đa 64 KiB, hàng đợi nhận tối đa 1.024 thông điệp và danh sách peer tối đa 64 địa chỉ. API `ConsensusP2pNode` xử lý thông điệp block/vote/certificate; API `P2pNode` cấp transport tổng quát. Nút cần cùng trạng thái ledger và tập validator ban đầu để đồng thuận.

## Ghi chú
Dự án này vẫn chỉ là một bản triển khai phục vụ học tập/demo, không phải blockchain dùng trong môi trường thực tế hay một hệ thống mật mã hoàn chỉnh.
Genesis hiện có hash xác định để các nút mới bắt đầu từ cùng một điểm. API `ConsensusEngine` xác minh phiếu biểu quyết Ed25519, hoàn tất block khi có hơn hai phần ba stake trong snapshot ký xác nhận, phạt/đình chỉ validator ký hash xung đột ở cùng độ cao, và hỗ trợ unstake có trì hoãn.

Đây chưa phải giải pháp an toàn cấp production: khóa riêng validator vẫn do người vận hành quản lý, việc đăng ký validator/stake và trạng thái ban đầu phải được cấu hình nhất quán thủ công, và CLI không cung cấp quản trị slashing. Giao dịch tạo bằng `transfer` cục bộ vẫn dùng hash demo, không được xem là xác thực quyền sở hữu và không thể được node P2P finalize; dùng `transfer-signed` cho giao dịch mạng. Các tệp ledger cũ có genesis không xác định cần được đặt lại hoặc đồng bộ từ cùng một trạng thái mới để tham gia mạng. Ledger mặc định là `.resonant-ledger.json`, consensus state là `.resonant-consensus.json`; đặt `RESONANT_LEDGER_PATH` hoặc `RESONANT_CONSENSUS_PATH` để dùng đường dẫn khác.

## Vì sao có v2
Phiên bản đầu tiên đã chứng minh được ý tưởng. v2 tập trung vào cấu trúc gọn gàng hơn, phân tách trách nhiệm rõ ràng hơn và bố cục mã thuận tiện hơn cho việc benchmark.
