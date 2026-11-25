from pathlib import Path
import tempfile
import unittest

import guest_password


class ApprovedPasswordTests(unittest.TestCase):
    def test_cached_fault_observation_requires_the_exact_approved_version(self):
        instance = {'desired': {'id': 'owned-guest', 'spec': {'login_revision': 2}}}
        credential = {'instance_id': 'owned-guest', 'revision': 2, 'password': 'a' * 32}
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            guest_password.prepare(lambda *_: credential, output, instance, lambda *_: None)
            guest_password.validate_cached(output, instance, 2)
            with self.assertRaises(RuntimeError):
                guest_password.validate_cached(output, instance, 1)
            instance['desired']['spec']['login_revision'] = 3
            with self.assertRaises(RuntimeError):
                guest_password.validate_cached(output, instance, 3)

    def test_failed_credential_request_cannot_create_an_approved_cache(self):
        instance = {'desired': {'id': 'owned-guest', 'spec': {'login_revision': 2}}}

        def unavailable(*_):
            raise RuntimeError('expected credential API refusal during injected failure')

        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            with self.assertRaises(RuntimeError):
                guest_password.prepare(unavailable, output, instance, lambda *_: None)
            with self.assertRaises(FileNotFoundError):
                guest_password.validate_cached(output, instance, 2)


if __name__ == '__main__':
    unittest.main()
