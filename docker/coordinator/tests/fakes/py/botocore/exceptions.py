"""The one botocore exception the entrypoint catches (see ../boto3)."""

# The slice of botocore's error response the stand-in raises: {"Error": {"Code": ...}}.
ErrorResponse = dict[str, dict[str, str]]


class ClientError(Exception):
    def __init__(self, error_response: ErrorResponse, operation_name: str) -> None:
        super().__init__(f"An error occurred ({error_response['Error']['Code']}) {operation_name}")
        self.response = error_response
