use std::{collections::VecDeque, net::{Ipv4Addr, SocketAddrV4}, ops::Range, os::fd::OwnedFd, sync::{mpsc, Arc, Mutex}};

use bytes::Bytes;
use log::{debug, trace};

use super::raw_socket::RawSocketSendHalf;

type Socket = super::socket::Socket<RawSocketSendHalf>;

const LINGERING_MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(30);
const ACTIVE_MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(60);
const CONNECT_TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_secs(3);

const CLIENT_PORT_RANGE: Range<u16> = 10000..60000;

struct SocketTable {
    raw_sock_send_half: RawSocketSendHalf,

    local_ip: Ipv4Addr,
    local_port_next: u16,
    remote_addr: SocketAddrV4,

    /// lingering sockets, with the time when it becomes lingering
    lingering_socks: VecDeque<(Socket, std::time::Instant)>,
    /// active socket, with the time when it becomes active
    active_sock: Option<(Socket, std::time::Instant)>,
    /// connecting socket, with the time when it starts connect
    connecting_sock: Option<(Socket, std::time::Instant)>,
}

impl SocketTable {
    fn map_mut<F>(&mut self, mut f: F) where F: FnMut(&mut Socket) {
        let mut f = |&mut (ref mut s, _)| { f(s) };
        self.active_sock.as_mut().map(&mut f);
        self.connecting_sock.as_mut().map(&mut f);
        self.lingering_socks.iter_mut().map(&mut f);
    }

    fn next_local_port(&mut self) -> u16 {
        self.local_port_next += 1;
        if self.local_port_next >= CLIENT_PORT_RANGE.end {
            self.local_port_next = CLIENT_PORT_RANGE.start;
        }
        return self.local_port_next;
    }

    fn maintain(&mut self) {
        let now = std::time::Instant::now();
        while !self.lingering_socks.is_empty()
            && now - self.lingering_socks.front().unwrap().1 > LINGERING_MAX_DURATION {
                debug!("Removing lingering socket {}", self.lingering_socks.front().unwrap().0);
                self.lingering_socks.pop_front();
            }

        if self.connecting_sock.as_ref().map_or(false, |x| x.0.ready()) {
            let connected_s = self.connecting_sock.take().unwrap().0;
            debug!("New connected socket {}", connected_s);
            if let Some((mut active_s, _)) = self.active_sock.take() {
                debug!("Inactivate socket {}", active_s);
                active_s.send_rst().unwrap();
                self.lingering_socks.push_back((active_s, now.clone()));
            }
            self.active_sock = Some((connected_s, now.clone()));
        }

        let need_new_connect =
            self.active_sock.as_ref().map_or(true, |x| now - x.1 > ACTIVE_MAX_DURATION)
            && self.connecting_sock.as_ref().map_or(true, |x| now - x.1 > CONNECT_TIMEOUT_DURATION);
        if need_new_connect {
            if let Some((connecting_s, _)) = &mut self.connecting_sock {
                debug!("Connect timeout {}", connecting_s);
                connecting_s.send_rst().unwrap();
            }
            let local_addr = SocketAddrV4::new(self.local_ip, self.next_local_port());
            let s = Socket::new_connect(local_addr, self.remote_addr, self.raw_sock_send_half.clone()).unwrap();
            debug!("New connecting socket {}", s);
            self.connecting_sock = Some((s, now));
        }
    }

    fn feed_packet(&mut self, buf: &Bytes, received_data_queue: &mpsc::Sender<Bytes>) {
        self.map_mut(|s| {
            if let Ok(data) = s.feed_packet(&buf) {
                if data.len() > 0 {
                    let data = buf.slice_ref(data);
                    received_data_queue.send(data).unwrap();
                }
            }
        });
        self.maintain();
    }

    fn send_data(&mut self, buf: &Bytes) {
        if let Some((s, _)) = &mut self.active_sock {
            s.send(&buf).unwrap();
        } else {
            trace!("No active socket, drop packet");
        }
    }
}


struct Client {
    raw_sock_fd: Arc<OwnedFd>,

    sock_table: Arc<Mutex<SocketTable>>,

    recv_thread: std::thread::JoinHandle<()>,
    maintain_thread: std::thread::JoinHandle<()>,

    received_data_queue_receiver: mpsc::Receiver<Bytes>,
}

// todo: drop

impl Client {
    pub fn new(raw_sock_fd: OwnedFd, local_ip: Ipv4Addr, remote_addr: SocketAddrV4) -> Client {
        let (raw_sock_fd, raw_sock_send_half, mut raw_sock_recv_half) =
            super::raw_socket::new_splitted_raw_socket(raw_sock_fd);
        let sock_table = Arc::new(Mutex::new(SocketTable {
            raw_sock_send_half,
            local_ip,
            local_port_next: CLIENT_PORT_RANGE.start,
            remote_addr,
            lingering_socks: VecDeque::new(),
            active_sock: None,
            connecting_sock: None,
        }));
        let (received_data_queue_sender, received_data_queue_receiver) =
            mpsc::channel::<Bytes>();

        let recv_thread = {
            let sock_table = sock_table.clone();
            std::thread::spawn(move || {
                loop {
                    let buf = raw_sock_recv_half.recv().unwrap();
                    sock_table.lock().unwrap().feed_packet(&buf, &received_data_queue_sender);
                }
            })
        };
        let maintain_thread = {
            let sock_table = sock_table.clone();
            std::thread::spawn(move || {
                loop {
                    sock_table.lock().unwrap().maintain();
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            })
        };

        Client {
            raw_sock_fd,
            sock_table,
            recv_thread,
            maintain_thread,
            received_data_queue_receiver,
        }
    }

    pub fn send_data(&self, buf: &Bytes) {
        self.sock_table.lock().unwrap().send_data(buf)
    }

}
