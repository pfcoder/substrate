#![allow(dead_code)]
#![allow(missing_docs)]
use futures::{
	ready,
	future::Future,
	stream::Stream,
	channel::{
		oneshot,
		mpsc::{Sender, Receiver, channel},
	},
};
use std::{
	pin::Pin,
	sync::Arc,
	task::{Context, Poll}
};
use sp_core::{
	crypto::{
		CryptoTypePublicPair,
		KeyTypeId,
	},
	traits::{
		BareCryptoStorePtr,
		BareCryptoStoreError,
	},
};
pub use sp_externalities::{Externalities, ExternalitiesExt};

const CHANNEL_SIZE: usize = 128;

pub enum RequestMethod {
	SignWith(KeyTypeId, CryptoTypePublicPair, Vec<u8>),
	HasKeys(Vec<(Vec<u8>, KeyTypeId)>),
	InsertUnknown(KeyTypeId, String, Vec<u8>),
}

pub struct KeystoreRequest {
	sender: oneshot::Sender<KeystoreResponse>,
	method: RequestMethod,
}

pub enum KeystoreResponse {
	SignWith(Result<Vec<u8>, BareCryptoStoreError>),
	HasKeys(bool),
	InsertUnknown(Result<(), ()>),
}

pub enum PendingFuture {
	SignWith(Pin<Box<dyn Future<Output = Result<Vec<u8>, BareCryptoStoreError>>>>),
	HasKeys(Pin<Box<dyn Future<Output = bool>>>),
	InsertUnknown(Pin<Box<dyn Future<Output = Result<(), ()>>>>),
}

struct PendingCall {
	future: PendingFuture,
	sender: oneshot::Sender<KeystoreResponse>,
}

pub struct KeystoreProxy {
	sender: Sender<KeystoreRequest>,
}

impl KeystoreProxy {
	pub fn new(sender: Sender<KeystoreRequest>) -> Self {
		KeystoreProxy {
			sender,
		}
	}

	fn send_request(&self, request: RequestMethod) -> oneshot::Receiver<KeystoreResponse> {
		let (request_sender, request_receiver) = oneshot::channel::<KeystoreResponse>();

		let request = KeystoreRequest {
			sender: request_sender,
			method: request,
		};
		let mut sender = self.sender.clone();
		sender.start_send(request);

		request_receiver
	}

	pub fn sign_with(
		&self,
		id: KeyTypeId,
		key: &CryptoTypePublicPair,
		msg: &[u8],
	) -> oneshot::Receiver<KeystoreResponse> {
		self.send_request(RequestMethod::SignWith(id, key.clone(), msg.to_vec()))
	}

	pub fn has_keys(
		&self,
		public_keys: &[(Vec<u8>, KeyTypeId)]
	) -> oneshot::Receiver<KeystoreResponse> {
		self.send_request(RequestMethod::HasKeys(public_keys.to_vec()))
	}

	pub fn insert_unknown(
		&self,
		key_type: KeyTypeId,
		suri: &str,
		public: &[u8]
	) -> oneshot::Receiver<KeystoreResponse> {
		self.send_request(RequestMethod::InsertUnknown(
			key_type,
			suri.to_string(),
			public.to_vec(),
		))
	}
}

pub struct KeystoreReceiver {
	receiver: Receiver<KeystoreRequest>,
	store: BareCryptoStorePtr,
	pending: Vec<PendingCall>,
}

impl KeystoreReceiver {
	pub fn new(store: BareCryptoStorePtr, receiver: Receiver<KeystoreRequest>) -> Self {
		KeystoreReceiver {
			receiver,
			store,
			pending: vec![],
		}
	}

	fn process_request(&mut self, request: KeystoreRequest) {
		let keystore = self.store.clone();
		match request.method {
			RequestMethod::SignWith(id, key, msg) => {
				let future = async move {
					keystore.read().sign_with(id, &key, &msg).await
				};

				self.pending.push(PendingCall {
					future: PendingFuture::SignWith(Box::pin(future)),
					sender: request.sender,
				});
			},
			RequestMethod::HasKeys(keys) => {
				let future = async move {
					keystore.read().has_keys(&keys).await
				};

				self.pending.push(PendingCall {
					future: PendingFuture::HasKeys(Box::pin(future)),
					sender: request.sender,
				});
			},
			RequestMethod::InsertUnknown(key_type, suri, pubkey) => {
				let future = async move {
					keystore.write().insert_unknown(
						key_type,
						suri.as_str(),
						&pubkey,
					).await
				};

				self.pending.push(PendingCall {
					future: PendingFuture::InsertUnknown(Box::pin(future)),
					sender: request.sender,
				});
			}
		}
	}

	fn poll_future(&self, cx: &mut Context, pending: PendingCall) {
		match pending.future {
			PendingFuture::SignWith(mut future) => {
				future.as_mut().poll(cx);
			},
			PendingFuture::HasKeys(mut future) => {
				future.as_mut().poll(cx);
			},
			PendingFuture::InsertUnknown(mut future) => {
				future.as_mut().poll(cx);
			}
		}
	}
}

impl Future for KeystoreReceiver {
	type Output = ();

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
		// for item in self.pending.into_iter() {
		// 	self.poll_future(cx, item);
		// }

		if let Some(request) = ready!(Pin::new(&mut self.receiver).poll_next(cx)) {
			self.process_request(request);
		}

		return Poll::Pending;
	}
}

sp_externalities::decl_extension! {
	/// The keystore extension to register/retrieve from the externalities.
	pub struct KeystoreProxyExt(Arc<KeystoreProxy>);
}

pub fn proxy(store: BareCryptoStorePtr) -> (KeystoreProxy, KeystoreReceiver) {
	let (sender, receiver) = channel::<KeystoreRequest>(CHANNEL_SIZE);
	(KeystoreProxy::new(sender), KeystoreReceiver::new(store, receiver))
}