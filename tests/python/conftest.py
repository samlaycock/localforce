"""Starts localforce for the test session, and connects simple-salesforce to it.

Build first with `cargo build`, or point LOCALFORCE_BIN at a binary. To run
against a real org instead, set SF_INSTANCE_URL and SF_ACCESS_TOKEN (see
tests/README.md).

simple-salesforce always uses https://, and builds its login URLs from a
domain (https://{domain}.salesforce.com). So locally it talks to localforce's
HTTPS port, trusting localforce's generated certificate, and the login tests
mount a requests adapter that sends *.salesforce.com to localforce.
"""

import os
import shutil
import socket
import subprocess
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
from urllib.parse import urlsplit, urlunsplit

import pytest
import requests
from requests.adapters import HTTPAdapter
from simple_salesforce import Salesforce

REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURES = REPO_ROOT / "tests" / "conformance" / "fixtures"
API_VERSION = "63.0"
USERNAME = "admin@localforce.test"
SECOND_USERNAME = "second@localforce.test"


@dataclass
class Target:
    """Where the tests run: localforce (`localforce=True`) or a real org."""

    localforce: bool
    instance_url: str
    access_token: str
    session: requests.Session
    #: The base URL logins go to, for the login tests (None: no login details).
    login_url: Optional[str] = None
    username: Optional[str] = None
    password: Optional[str] = None
    client_id: Optional[str] = None
    client_secret: Optional[str] = None
    second_username: Optional[str] = None
    extra: dict = field(default_factory=dict)

    def client(self, **kwargs) -> Salesforce:
        """A new simple-salesforce client on the main session."""
        return Salesforce(
            instance_url=self.instance_url,
            session_id=self.access_token,
            session=self.session,
            version=API_VERSION,
            **kwargs,
        )


class LoginRedirect(HTTPAdapter):
    """Sends requests for *.salesforce.com (simple-salesforce's login URLs) to `base`."""

    def __init__(self, base: str):
        super().__init__()
        self.base = urlsplit(base)

    def send(self, request, *args, **kwargs):
        url = urlsplit(request.url)
        if url.hostname and url.hostname.endswith(".salesforce.com"):
            request.url = urlunsplit((self.base.scheme, self.base.netloc, url.path, url.query, url.fragment))
        return super().send(request, *args, **kwargs)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _start_localforce():
    binary = os.environ.get("LOCALFORCE_BIN") or str(REPO_ROOT / "target" / "debug" / "localforce")
    if not Path(binary).exists():
        raise RuntimeError(f"no localforce binary at {binary}: run `cargo build` first, or set LOCALFORCE_BIN")
    port, tls_port = _free_port(), _free_port()
    data_dir = tempfile.mkdtemp(prefix="localforce-python-")
    log = open(Path(__file__).parent / "localforce.log", "wb")
    proc = subprocess.Popen(
        [
            binary,
            "--host", "127.0.0.1",
            "--port", str(port),
            "--tls-port", str(tls_port),
            "--data-dir", data_dir,
            "--config", str(FIXTURES / "localforce.toml"),
            "--metadata", str(FIXTURES / "sfdx"),
            # Like Salesforce, refuse tokens localforce didn't issue.
            "--strict-sessions",
        ],
        stdout=log,
        stderr=subprocess.STDOUT,
        env={**os.environ, "NO_COLOR": "1"},
    )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"localforce exited with status {proc.returncode}; see tests/python/localforce.log")
        try:
            if requests.get(f"http://127.0.0.1:{port}/services/data", timeout=1).ok:
                break
        except requests.RequestException:
            time.sleep(0.05)
    else:
        proc.kill()
        raise RuntimeError("localforce did not start")
    return proc, port, tls_port, data_dir, log


@pytest.fixture(scope="session")
def target():
    if os.environ.get("SF_INSTANCE_URL") and os.environ.get("SF_ACCESS_TOKEN"):
        session = requests.Session()
        yield Target(
            localforce=False,
            instance_url=os.environ["SF_INSTANCE_URL"],
            access_token=os.environ["SF_ACCESS_TOKEN"],
            session=session,
            login_url=os.environ.get("SF_LOGIN_URL"),
            username=os.environ.get("SF_USERNAME"),
            password=os.environ.get("SF_PASSWORD"),
            client_id=os.environ.get("SF_CLIENT_ID"),
            client_secret=os.environ.get("SF_CLIENT_SECRET"),
            second_username=os.environ.get("LF_SECOND_USERNAME"),
        )
        return

    proc, port, tls_port, data_dir, log = _start_localforce()
    try:
        session = requests.Session()
        session.verify = str(Path(data_dir) / "localforce-cert.pem")
        base = f"https://127.0.0.1:{tls_port}"
        token = session.post(
            f"{base}/services/oauth2/token",
            data={
                "grant_type": "password",
                "client_id": "localforce-tests",
                "client_secret": "any-secret",
                "username": USERNAME,
                "password": "any-password",
            },
        ).json()
        yield Target(
            localforce=True,
            instance_url=token["instance_url"],
            access_token=token["access_token"],
            session=session,
            login_url=base,
            username=USERNAME,
            password="any-password",
            client_id="localforce-tests",
            client_secret="any-secret",
            second_username=SECOND_USERNAME,
            extra={"http_url": f"http://127.0.0.1:{port}", "cert": session.verify},
        )
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        log.close()
        shutil.rmtree(data_dir, ignore_errors=True)


@pytest.fixture(scope="session")
def sf(target) -> Salesforce:
    return target.client()


@pytest.fixture(scope="session")
def users(target, sf) -> dict:
    """`{"user": the session user's Id, "user2": another active user's Id or None}`."""
    info = sf.oauth2("userinfo")
    user2 = None
    if target.second_username:
        res = sf.query(f"SELECT Id FROM User WHERE Username = '{target.second_username}' AND IsActive = true")
        user2 = res["records"][0]["Id"] if res["records"] else None
    return {"user": info["user_id"], "user2": user2}


@pytest.fixture
def login_session(target) -> requests.Session:
    """A session whose https://*.salesforce.com requests go to the target's login URL."""
    if not target.login_url:
        pytest.skip("needs SF_LOGIN_URL (and login details) to test logging in")
    session = requests.Session()
    session.verify = target.session.verify
    if target.localforce:
        session.mount("https://", LoginRedirect(target.login_url))
    return session


@pytest.fixture
def login_domain(target) -> str:
    """The `domain` simple-salesforce logs in to: `login` locally (redirected), else from SF_LOGIN_URL."""
    if target.localforce:
        return "login"
    host = urlsplit(target.login_url or "").hostname or ""
    if not host.endswith(".salesforce.com"):
        pytest.skip("SF_LOGIN_URL isn't a *.salesforce.com URL")
    return host[: -len(".salesforce.com")]


@pytest.fixture(scope="session")
def tag() -> str:
    """A tag unique to this run, for names and filters."""
    from spec import new_tag

    return new_tag()
