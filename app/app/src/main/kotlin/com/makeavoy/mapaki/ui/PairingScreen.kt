package com.makeavoy.mapaki.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

@Composable
fun PairingScreen(
    error: String?,
    isSubmitting: Boolean,
    onSubmit: (String) -> Unit,
) {
    var code by remember { mutableStateOf("") }

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            modifier = Modifier.padding(32.dp).widthIn(max = 360.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Mapaki", style = MaterialTheme.typography.headlineMedium)
            Spacer(Modifier.height(8.dp))
            Text(
                "Enter the pairing code printed by mapaki-daemon on startup.",
                style = MaterialTheme.typography.bodyMedium,
                textAlign = TextAlign.Center,
            )
            Spacer(Modifier.height(24.dp))
            OutlinedTextField(
                value = code,
                onValueChange = { code = it },
                label = { Text("Pairing code") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            if (error != null) {
                Spacer(Modifier.height(8.dp))
                Text(error, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            }
            Spacer(Modifier.height(16.dp))
            Button(
                onClick = { onSubmit(code) },
                enabled = code.isNotBlank() && !isSubmitting,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (isSubmitting) "Pairing…" else "Pair")
            }
        }
    }
}
